//! What else the host was running while a round ran. A round capped on wall clock can be
//! slow because the box was busy with work it does not own; this samples `/proc` for the
//! length of a run and says so, so the red is read as the box's and not the beads'.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Competing CPU, as a percent of the host's capacity over the run, at which the run says it
/// was contended.
pub const CONTENDED_PCT: u64 = 25;
pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
const TOP: usize = 5;

/// Total jiffies on the first line of `/proc/stat`.
pub fn parse_total(stat: &str) -> Option<u64> {
    let line = stat.lines().next()?;
    let mut it = line.split_whitespace();
    (it.next()? == "cpu").then_some(())?;
    Some(it.take(8).filter_map(|v| v.parse::<u64>().ok()).sum())
}

/// (utime + stime, start time) from a `/proc/<pid>/stat` body.
pub fn parse_pid_ticks(stat: &str) -> Option<(u64, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some((ticks, f.get(19)?.parse().ok()?))
}

/// The round's own VM: a process whose argv carries `-id <handle>` (how the hypervisor
/// starts a guest).
pub fn is_round_vm(argv: &[String], handle: &str) -> bool {
    argv.windows(2).any(|w| w[0] == "-id" && w[1] == handle)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    ticks: u64,
    comm: String,
    round: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub total: u64,
    pub round: u64,
    pub vm_found: bool,
    pub others: Vec<(String, u64)>,
}

impl Report {
    fn pct(&self, t: u64) -> u64 {
        if self.total == 0 {
            0
        } else {
            t * 100 / self.total
        }
    }

    pub fn competing(&self) -> u64 {
        self.others.iter().map(|(_, t)| t).sum()
    }

    pub fn contended(&self) -> bool {
        self.vm_found && self.pct(self.competing()) >= CONTENDED_PCT
    }

    pub fn render(&self) -> String {
        let top: Vec<String> = self.others.iter().take(TOP).map(|(c, t)| format!("{c}={}%", self.pct(*t))).collect();
        let share = if self.vm_found { format!("{}%", self.pct(self.round)) } else { "unknown (round VM process not seen)".into() };
        format!(
            "{}round held {share} of the host CPU; other work on the host {}%{}",
            if self.contended() { "CONTENDED: " } else { "" },
            self.pct(self.competing()),
            if top.is_empty() { String::new() } else { format!(" — {}", top.join(" ")) }
        )
    }
}

/// Accumulates CPU use per process across samples, so a build that started and finished
/// between two looks is still counted.
pub struct Sampler {
    proc_root: PathBuf,
    handle: String,
    first_total: Option<u64>,
    last_total: u64,
    seen: BTreeMap<(u32, u64), Seen>,
    base: BTreeMap<(u32, u64), u64>,
}

impl Sampler {
    pub fn new(proc_root: &Path, handle: &str) -> Sampler {
        Sampler { proc_root: proc_root.to_path_buf(), handle: handle.to_string(), first_total: None, last_total: 0, seen: BTreeMap::new(), base: BTreeMap::new() }
    }

    pub fn sample(&mut self) {
        let Some(total) = std::fs::read_to_string(self.proc_root.join("stat")).ok().and_then(|s| parse_total(&s)) else { return };
        let first = self.first_total.is_none();
        self.first_total.get_or_insert(total);
        self.last_total = total;
        let Ok(rd) = std::fs::read_dir(&self.proc_root) else { return };
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_string_lossy().parse::<u32>().ok() else { continue };
            let Some((ticks, start)) = std::fs::read_to_string(e.path().join("stat")).ok().and_then(|s| parse_pid_ticks(&s)) else { continue };
            let key = (pid, start);
            if first {
                self.base.insert(key, ticks);
            }
            let comm = std::fs::read_to_string(e.path().join("comm")).map(|c| c.trim().to_string()).unwrap_or_default();
            let round = if comm.starts_with("kvm") || comm.starts_with("qemu") {
                let argv: Vec<String> = std::fs::read(e.path().join("cmdline"))
                    .map(|raw| raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
                    .unwrap_or_default();
                is_round_vm(&argv, &self.handle)
            } else {
                false
            };
            self.seen.insert(key, Seen { ticks, comm, round });
        }
    }

    pub fn report(&self) -> Report {
        let mut r = Report { total: self.last_total.saturating_sub(self.first_total.unwrap_or(0)), ..Report::default() };
        let mut by_comm: BTreeMap<String, u64> = BTreeMap::new();
        for (k, s) in &self.seen {
            let used = s.ticks.saturating_sub(self.base.get(k).copied().unwrap_or(0));
            if s.round {
                r.vm_found = true;
                r.round += used;
            } else if used > 0 {
                *by_comm.entry(s.comm.clone()).or_default() += used;
            }
        }
        r.others = by_comm.into_iter().collect();
        r.others.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        r
    }
}

/// Samples the host from a thread for as long as it lives, printing a LOAD line every
/// `REPORT_EVERY` (a run killed at its cap never reaches an end-of-run line) and a last one
/// on `finish`.
pub struct Watch {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Report>>,
}

impl Watch {
    pub fn start(handle: &str) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let mut s = Sampler::new(Path::new("/proc"), handle);
        let thread = std::thread::spawn(move || {
            let (mut next_sample, mut next_report) = (Instant::now(), Instant::now() + REPORT_EVERY);
            while !flag.load(Ordering::Relaxed) {
                if Instant::now() >= next_sample {
                    s.sample();
                    next_sample = Instant::now() + SAMPLE_EVERY;
                }
                if Instant::now() >= next_report {
                    eprintln!("round-vm run: LOAD: {}", s.report().render());
                    next_report = Instant::now() + REPORT_EVERY;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            s.sample();
            s.report()
        });
        Watch { stop, thread: Some(thread) }
    }

    pub fn finish(mut self) -> Option<Report> {
        self.stop.store(true, Ordering::Relaxed);
        let r = self.thread.take()?.join().ok()?;
        eprintln!("round-vm run: LOAD: {}", r.render());
        Some(r)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::TempDir;
    use std::fs;

    fn plant(root: &Path, pid: u32, comm: &str, start: u64, ticks: u64, cmdline: &str) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(d.join("stat"), format!("{pid} ({comm}) S 1 0 0 0 0 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 {start}\n")).unwrap();
        fs::write(d.join("cmdline"), cmdline.replace(' ', "\0")).unwrap();
    }

    fn host_total(root: &Path, total: u64) {
        fs::write(root.join("stat"), format!("cpu  {total} 0 0 0 0 0 0 0 0 0\n")).unwrap();
    }

    #[test]
    fn parsers_read_the_proc_formats() {
        assert_eq!(parse_total("cpu  1 2 3 4 5 6 7 8 9 10\ncpu0 1\n"), Some(36));
        assert_eq!(parse_total("intr 1\n"), None);
        assert_eq!(parse_pid_ticks("5 (a (b) c) S 1 0 0 0 0 0 0 0 0 0 7 3 0 0 20 0 1 0 99\n"), Some((10, 99)));
        assert!(is_round_vm(&["kvm".into(), "-id".into(), "120".into()], "120"));
        assert!(!is_round_vm(&["kvm".into(), "-id".into(), "1200".into()], "120"));
    }

    #[test]
    fn a_hog_beside_the_round_is_named_and_the_round_is_called_contended() {
        let d = TempDir::new("load-contended");
        let root = d.join("proc");
        fs::create_dir_all(&root).unwrap();
        host_total(&root, 1000);
        plant(&root, 10, "kvm", 1, 0, "kvm -id 120 -name round");
        plant(&root, 11, "hunk", 2, 0, "hunk");
        plant(&root, 12, "bash", 3, 0, "bash");
        let mut s = Sampler::new(&root, "120");
        s.sample();
        host_total(&root, 2000);
        plant(&root, 10, "kvm", 1, 200, "kvm -id 120 -name round");
        plant(&root, 11, "hunk", 2, 600, "hunk");
        plant(&root, 13, "rustc", 4, 80, "rustc");
        s.sample();
        let r = s.report();
        assert_eq!((r.total, r.round, r.vm_found), (1000, 200, true));
        assert_eq!(r.others, vec![("hunk".to_string(), 600), ("rustc".to_string(), 80)]);
        assert!(r.contended());
        let line = r.render();
        assert!(line.starts_with("CONTENDED: round held 20% of the host CPU"), "{line}");
        assert!(line.contains("hunk=60%"), "{line}");
    }

    #[test]
    fn a_quiet_host_is_not_contended_and_a_missing_vm_is_not_judged() {
        let d = TempDir::new("load-quiet");
        let root = d.join("proc");
        fs::create_dir_all(&root).unwrap();
        host_total(&root, 0);
        plant(&root, 10, "kvm", 1, 0, "kvm -id 120");
        plant(&root, 11, "hunk", 2, 0, "hunk");
        let mut s = Sampler::new(&root, "120");
        s.sample();
        host_total(&root, 1000);
        plant(&root, 10, "kvm", 1, 700, "kvm -id 120");
        plant(&root, 11, "hunk", 2, 50, "hunk");
        s.sample();
        assert!(!s.report().contended());
        let mut other = Sampler::new(&root, "999");
        other.sample();
        host_total(&root, 2000);
        plant(&root, 11, "hunk", 2, 900, "hunk");
        other.sample();
        let r = other.report();
        assert!(!r.vm_found && !r.contended(), "no VM process, no verdict: {}", r.render());
        assert!(r.render().contains("unknown"));
    }
}
