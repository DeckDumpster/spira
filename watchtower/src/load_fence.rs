//! `--load-fence-check` — a CPU-heavy build (rustc, cargo, a linker, hunk) that no aeon or
//! builder session owns competes with a live round at full priority. This pass lowers every
//! such process to nice 19 and the idle I/O class wherever it came from, and logs where it
//! came from (cgroup unit, ancestry) so an ownerless build can be traced afterwards.

use crate::log::log;
use std::fs;
use std::path::Path;

pub const DEFAULT_COMMS: &str = "rustc cargo cc1 cc1plus ld lld hunk";
pub const DEFAULT_OWNER_MARK: &str = "spira";
const NICE_FLOOR: i64 = 19;
const IOPRIO_CLASS_IDLE: i64 = 3;
const IOPRIO_WHO_PROCESS: i64 = 1;

pub struct Cfg {
    pub comms: Vec<String>,
    /// A cgroup path containing this names a harness-owned process; anything else is ownerless.
    pub owner_mark: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub comm: String,
    pub nice: i64,
    pub ppid: u32,
    pub cgroup: String,
}

impl Proc {
    pub fn unit(&self) -> &str {
        self.cgroup.rsplit('/').find(|s| !s.is_empty()).unwrap_or("-")
    }
}

fn stat_fields(pid_dir: &Path) -> Option<(i64, u32)> {
    let stat = fs::read_to_string(pid_dir.join("stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f.get(16)?.parse().ok()?, f.get(1)?.parse().ok()?))
}

fn read_proc(pid_dir: &Path, pid: u32) -> Option<Proc> {
    let comm = fs::read_to_string(pid_dir.join("comm")).ok()?.trim().to_string();
    let (nice, ppid) = stat_fields(pid_dir)?;
    let cgroup = fs::read_to_string(pid_dir.join("cgroup"))
        .ok()
        .and_then(|t| t.lines().last().map(|l| l.rsplit(':').next().unwrap_or("").to_string()))
        .unwrap_or_default();
    Some(Proc { pid, comm, nice, ppid, cgroup })
}

pub fn scan(proc_root: &Path, cfg: &Cfg) -> Vec<Proc> {
    let Ok(rd) = fs::read_dir(proc_root) else { return Vec::new() };
    let mut v: Vec<Proc> = rd
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_string_lossy().parse::<u32>().ok()?;
            let p = read_proc(&e.path(), pid)?;
            cfg.comms.iter().any(|c| *c == p.comm).then_some(p)
        })
        .collect();
    v.sort_by_key(|p| p.pid);
    v
}

pub fn ownerless(p: &Proc, cfg: &Cfg) -> bool {
    !p.cgroup.contains(&cfg.owner_mark)
}

/// `comm`s from `pid` up through its parents, nearest first, at most `depth` deep.
pub fn ancestry(proc_root: &Path, pid: u32, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < depth && cur > 1 {
        let d = proc_root.join(cur.to_string());
        let Some(p) = read_proc(&d, cur) else { break };
        out.push(p.comm);
        cur = p.ppid;
    }
    out
}

fn io_class(pid: u32) -> Option<i64> {
    // SAFETY: ioprio_get takes two integers and touches no memory of ours.
    let r = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, pid as i64) };
    (r >= 0).then_some((r as i64) >> 13)
}

/// Lowers `pid` to nice 19 and the idle I/O class; true when anything changed.
pub fn fence(p: &Proc) -> bool {
    let mut changed = false;
    if p.nice < NICE_FLOOR {
        // SAFETY: setpriority on a pid number; failure (gone, not ours) is the return value.
        changed |= unsafe { libc::setpriority(libc::PRIO_PROCESS, p.pid, NICE_FLOOR as libc::c_int) } == 0;
    }
    if io_class(p.pid).is_some_and(|c| c != IOPRIO_CLASS_IDLE) {
        // SAFETY: ioprio_set takes three integers and touches no memory of ours.
        changed |= unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, p.pid as i64, IOPRIO_CLASS_IDLE << 13) } == 0;
    }
    changed
}

pub fn run(proc_root: &Path, cfg: &Cfg) -> usize {
    let found = scan(proc_root, cfg);
    let mut fenced = 0;
    for p in &found {
        if !fence(p) {
            continue;
        }
        fenced += 1;
        let who = if ownerless(p, cfg) { "OWNERLESS" } else { "owned" };
        log(&format!(
            "watchtower: load-fence-check — fenced {} pid {} ({who}, unit {}, via {})",
            p.comm,
            p.pid,
            p.unit(),
            ancestry(proc_root, p.pid, 6).join(" < ")
        ));
    }
    log(&format!("watchtower: load-fence-check — {} watched process(es), {fenced} newly fenced", found.len()));
    fenced
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn fake(root: &Path, pid: u32, comm: &str, nice: i64, ppid: u32, cgroup: &str) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(d.join("stat"), format!("{pid} ({comm}) S {ppid} 0 0 0 0 0 0 0 0 0 0 0 0 0 20 {nice} 1 0\n")).unwrap();
        fs::write(d.join("cgroup"), format!("0::{cgroup}\n")).unwrap();
    }

    fn cfg(comms: &str) -> Cfg {
        Cfg { comms: comms.split_whitespace().map(String::from).collect(), owner_mark: "spira".into() }
    }

    #[test]
    fn only_watched_comms_are_found_with_their_nice_and_owner() {
        let dir = testkit::TempDir::new("wt-load-fence-scan");
        let r = dir.join("proc");
        fake(&r, 10, "rustc", 0, 5, "/user.slice/session-3.scope");
        fake(&r, 11, "bash", 0, 1, "/user.slice/session-3.scope");
        fake(&r, 12, "cargo", 19, 1, "/spira.slice/spira-aeon-x.scope");
        let c = cfg("rustc cargo");
        let found = scan(&r, &c);
        assert_eq!(found.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![10, 12]);
        assert_eq!((found[0].nice, found[0].ppid, found[0].unit()), (0, 5, "session-3.scope"));
        assert!(ownerless(&found[0], &c));
        assert!(!ownerless(&found[1], &c), "a process in a harness unit is owned");
    }

    #[test]
    fn ancestry_follows_parents_nearest_first() {
        let dir = testkit::TempDir::new("wt-load-fence-anc");
        let r = dir.join("proc");
        fake(&r, 10, "rustc", 0, 9, "/x");
        fake(&r, 9, "cargo", 0, 8, "/x");
        fake(&r, 8, "tmux", 0, 1, "/x");
        assert_eq!(ancestry(&r, 10, 6), vec!["rustc", "cargo", "tmux"]);
        assert_eq!(ancestry(&r, 10, 2), vec!["rustc", "cargo"]);
    }

    #[test]
    fn a_real_process_is_lowered_and_a_fenced_one_is_left_alone() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().unwrap();
        let c = cfg("sleep");
        let mine: Vec<Proc> = scan(Path::new("/proc"), &c).into_iter().filter(|p| p.pid == child.id()).collect();
        assert_eq!(mine.len(), 1, "the planted offender must be found before the fence is believed");
        assert!(mine[0].nice < NICE_FLOOR);
        assert!(fence(&mine[0]), "an unfenced process changes");
        let after = scan(Path::new("/proc"), &c).into_iter().find(|p| p.pid == child.id()).unwrap();
        assert_eq!(after.nice, NICE_FLOOR);
        assert_eq!(io_class(after.pid), Some(IOPRIO_CLASS_IDLE));
        assert!(!fence(&after), "an already fenced process is not touched again");
        let _ = child.kill();
        let _ = child.wait();
    }
}
