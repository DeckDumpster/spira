//! Names what a stalled queue is blocked on: every process holding a landing or gate lock
//! file (found by scanning `<proc>/*/fd`, never by asking the lock), with its argv0 and age.
//! A holder that is not a gate/lander process is a leaked-lock suspect — a child that
//! inherited the fd and outlived the locked section.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// argv0 basenames that legitimately hold these locks.
pub const LEGIT: &[&str] = &[
    "gate", "gate-run", "landing-pass", "queue", "batcher-cut", "batcher", "testenv", "sentinel", "watchtower",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub lock: PathBuf,
    pub pid: u32,
    pub argv0: String,
    pub age_s: i64,
}

impl Holder {
    pub fn suspect(&self) -> bool {
        !LEGIT.contains(&self.argv0.as_str())
    }
}

fn lock_names_in(dir: &Path, out: &mut BTreeSet<PathBuf>, keep: impl Fn(&str) -> bool) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".lock") && keep(&n) && e.path().is_file() {
            out.insert(e.path());
        }
    }
}

/// The lock files the landing and gate paths take that exist now: the landing lock and
/// landing-pass lock, gate admission slots, per-tree gate locks, per-repo queue/step locks.
pub fn lock_files(run: &Path, queue_dir: &Path) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    lock_names_in(run, &mut set, |n| n == "landing.lock" || n == "landing-pass.lock");
    lock_names_in(&run.join("gate-admission"), &mut set, |_| true);
    lock_names_in(&run.join("worktree"), &mut set, |n| n.starts_with(".gate."));
    if let Ok(rd) = fs::read_dir(queue_dir) {
        for e in rd.flatten() {
            lock_names_in(&e.path(), &mut set, |n| n == "lock.lock" || n == "step.lock");
            for f in ["lock", "step.lock"] {
                let p = e.path().join(f);
                if p.is_file() {
                    set.insert(p);
                }
            }
        }
    }
    set.into_iter().collect()
}

fn argv0(pid_dir: &Path) -> String {
    let raw = fs::read(pid_dir.join("cmdline")).unwrap_or_default();
    let first = raw.split(|b| *b == 0).next().unwrap_or(&[]);
    let s = String::from_utf8_lossy(first).into_owned();
    let base = s.rsplit('/').next().unwrap_or("").to_string();
    if base.is_empty() {
        fs::read_to_string(pid_dir.join("comm")).map(|c| c.trim().to_string()).unwrap_or_else(|_| "?".into())
    } else {
        base
    }
}

/// Every process under `proc_root` with an open fd resolving to one of `locks`. Age is the
/// time since the `<proc>/<pid>` entry was created, which for a real /proc is process start.
pub fn holders(proc_root: &Path, locks: &[PathBuf], now: i64) -> Vec<Holder> {
    let want: BTreeSet<PathBuf> = locks.iter().map(|l| fs::canonicalize(l).unwrap_or_else(|_| l.clone())).collect();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(proc_root) else { return out };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<u32>().ok() else { continue };
        let Ok(fds) = fs::read_dir(e.path().join("fd")) else { continue };
        let mut seen = BTreeSet::new();
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else { continue };
            if want.contains(&target) && seen.insert(target.clone()) {
                let age_s = fs::metadata(e.path())
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| (now - d.as_secs() as i64).max(0))
                    .unwrap_or(0);
                out.push(Holder { lock: target, pid, argv0: argv0(&e.path()), age_s });
            }
        }
    }
    out.sort_by(|a, b| (&a.lock, a.pid).cmp(&(&b.lock, b.pid)));
    out
}

pub fn render(hs: &[Holder]) -> String {
    let mut s = String::new();
    for h in hs {
        s.push_str(&format!(
            "{}  pid {}  argv0 {}  age {}s{}\n",
            h.lock.display(),
            h.pid,
            h.argv0,
            h.age_s,
            if h.suspect() { "  LEAKED-LOCK SUSPECT (not a gate/lander process)" } else { "" }
        ));
    }
    s
}

pub struct Ctx<'a> {
    pub run: &'a Path,
    pub queue_dir: &'a Path,
    pub proc_root: &'a Path,
    pub db: &'a str,
    pub home_repo: &'a str,
    pub incident_sh: &'a str,
}

/// Called only once a stall is established. Files nothing when no lock is held: then the
/// stall is not a lock stall and the throttle's own finding stands alone.
pub fn run(now: i64, stalled_for: &str, ctx: &Ctx) {
    let locks = lock_files(ctx.run, ctx.queue_dir);
    let hs = holders(ctx.proc_root, &locks, now);
    if hs.is_empty() {
        log(&format!("watchtower: lock-holders-check — stalled ({stalled_for}) but none of {} lock files is held", locks.len()));
        return;
    }
    let suspects = hs.iter().filter(|h| h.suspect()).count();
    log(&format!("watchtower: lock-holders-check — stalled ({stalled_for}); {} holder(s), {suspects} suspect", hs.len()));
    if !incident::is_usable(ctx.incident_sh) {
        return;
    }
    let body = format!(
        "The queue has stalled ({stalled_for}). Processes holding landing/gate lock files:\n\n{}\nA holder that is not a gate/lander process inherited the lock fd and outlived the locked section; killing it releases the lock.\n",
        render(&hs)
    );
    let f = Finding::new(ctx.db, ctx.home_repo, "QUEUE STALL: lock holders", &body)
        .priority(if suspects > 0 { 1 } else { 2 })
        .reference("incident:queue-lock-holders")
        .cause("queue-lock-holders");
    incident::file(ctx.incident_sh, &f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fake_proc(root: &Path, pid: u32, argv0: &str, lock: &Path) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(d.join("fd")).unwrap();
        fs::write(d.join("cmdline"), format!("/usr/bin/{argv0}\0--x\0")).unwrap();
        symlink(lock, d.join("fd/9")).unwrap();
    }

    #[test]
    fn a_lock_held_by_a_stray_process_is_reported_as_a_suspect_and_a_lander_is_not() {
        let dir = testkit::TempDir::new("wt-lock-holders");
        let run = dir.join("run");
        fs::create_dir_all(run.join("gate-admission")).unwrap();
        fs::create_dir_all(run.join("queue/spira")).unwrap();
        fs::write(run.join("landing.lock"), "").unwrap();
        fs::write(run.join("gate-admission/slot.1.lock"), "").unwrap();
        fs::write(run.join("queue/spira/lock"), "").unwrap();
        let procr = dir.join("proc");
        fake_proc(&procr, 41, "conmon", &run.join("landing.lock"));
        fake_proc(&procr, 42, "gate", &run.join("gate-admission/slot.1.lock"));
        fake_proc(&procr, 43, "dolt", &dir.join("unrelated.lock"));

        let locks = lock_files(&run, &run.join("queue"));
        assert_eq!(locks.len(), 3);
        let hs = holders(&procr, &locks, 0);
        assert_eq!(hs.len(), 2, "{hs:?}");
        let stray = hs.iter().find(|h| h.pid == 41).unwrap();
        assert_eq!(stray.argv0, "conmon");
        assert!(stray.suspect());
        assert!(!hs.iter().find(|h| h.pid == 42).unwrap().suspect());
        assert!(render(&hs).contains("LEAKED-LOCK SUSPECT"));
    }

    #[test]
    fn no_holder_is_found_when_nothing_holds_a_lock() {
        let dir = testkit::TempDir::new("wt-lock-holders-none");
        fs::write(dir.join("landing.lock"), "").unwrap();
        fs::create_dir_all(dir.join("proc/7/fd")).unwrap();
        let locks = lock_files(dir.path(), &dir.join("queue"));
        assert_eq!(locks.len(), 1);
        assert!(holders(&dir.join("proc"), &locks, 0).is_empty());
    }
}
