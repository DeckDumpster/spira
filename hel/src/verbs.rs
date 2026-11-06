//! The chokepoint verbs other crates call in-process (sentinel, landing-pass), with the same
//! bodies `hel <verb>` runs: one definition, no `hel` child. A unit stop goes through the
//! `Units` port, never a `systemctl` child (law-every-workflow-is-a-state-machine).

use std::path::Path;
use std::sync::Arc;

use runtime::units::{UnitHandle, Units};

use crate::ports::World;
use crate::real::{self, Real};
use crate::{locate_home, reap};

/// `hel holder-alive <id>`: a hold pidfile, a live dwarf pidfile or a live lease names a
/// holder. Err only when the run directory does not resolve.
pub fn holder_alive(id: &str) -> Result<bool, String> {
    Ok(reap::holder_alive(&real::run_dir()?, id))
}

/// `hel stop-holder <id>`: stop the unit of every process still working in the bead's
/// worktree, through `units`, then wait for them to leave (20 polls, 500 ms apart).
pub fn stop_holder(id: &str, units: &dyn Units) -> Result<(), String> {
    let run = real::run_dir()?;
    stop_holder_in(Path::new("/proc"), &run, id, units, 20, &|| std::thread::sleep(std::time::Duration::from_millis(500)) /* waits-on: worktree occupants exiting; cap 20 polls */)
}

/// [`stop_holder`] over a given `/proc` and run directory, for tests.
pub fn stop_holder_in(proc_root: &Path, run: &Path, id: &str, units: &dyn Units, tries: u32, settle: &dyn Fn()) -> Result<(), String> {
    let work = run.join("worktree").join(id);
    let mut failed: Vec<String> = Vec::new();
    let mut stop = |unit: &str| {
        if let Err(e) = units.stop(&UnitHandle::named(unit)) {
            failed.push(format!("{unit}: {}", e.0));
        }
    };
    let r = reap::stop_occupants(proc_root, &work, &mut stop, tries, settle);
    match r {
        Ok(()) => Ok(()),
        Err(pids) if failed.is_empty() => Err(format!("{id} still has live process(es) {pids:?} in its worktree after the unit stop")),
        Err(pids) => Err(format!("{id} still has live process(es) {pids:?} in its worktree; unit stop failed: {}", failed.join("; "))),
    }
}

/// The chokepoint's `Real`, minimal: no context read, so a caller handing a repo path never
/// depends on the repo-name registry resolving.
pub fn real_world(status: Option<&str>) -> Result<Real, String> {
    let home = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), std::env::var(spira_config::RELEASE_ENV).ok().as_deref()).ok_or("cannot find lib.sh (set SPIRA_HOME or SPIRA_RELEASE)")?;
    Ok(Real::minimal(home, real::run_dir()?, status.map(str::to_string)))
}

/// `hel destroy-worktree [--status-from <f>] <id> <path> <repo> <why>`: `Ok(true)` removed (or
/// nothing to remove), `Ok(false)` refused or failed, `Err` what the CLI would die with.
pub fn destroy_worktree(status_from: Option<&str>, id: &str, path: &str, repo: &str, why: &str) -> Result<bool, String> {
    let w = real_world(status_from)?;
    Ok(w.destroy_worktree(id, Path::new(path), Path::new(repo), why))
}

/// How one `reap-landed-branch` ended, before it is folded into the CLI's exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaped {
    Done,
    Queued,
    Failed(String),
}

/// The body of `hel reap-landed-branch`, against a given world. `caller` names who asked in the
/// reap log; the diagnostic line goes to stderr.
pub fn reap_landed_branch_on(w: &Real, id: &str, branch: &str, repo: &str, why: &str, caller: &str) -> Reaped {
    let repo = Path::new(repo);
    let base_info = w.base(repo);
    let base_ref = base_info.as_ref().map(|b| b.landref.as_str());
    let remote = base_info.as_ref().and_then(|b| b.remote.as_deref());
    let (run, reaplog_path) = (w.run(), w.reaplog_path());
    let log = |m: &str| eprintln!("{} spira: {m}", reap::utc_now());
    let label_remove = |id: &str, label: &str| w.label_remove(id, label);
    match reap::reap_landed_branch(&run, &reaplog_path, id, branch, repo, why, caller, base_ref, remote, w, &log, &label_remove) {
        reap::ReapOutcome::Done => Reaped::Done,
        reap::ReapOutcome::Queued => Reaped::Queued,
        reap::ReapOutcome::Failed(m) => Reaped::Failed(m),
    }
}

/// `hel reap-landed-branch` for an in-process caller: `Ok(())` reaped; `Err` carries what the
/// CLI printed (`certified-queued`, the failure) or, for a context the CLI died on, its reason.
pub fn reap_landed_branch(status_from: Option<&str>, id: &str, branch: &str, repo: &str, why: &str) -> Result<(), String> {
    let w = real_world(status_from)?;
    match reap_landed_branch_on(&w, id, branch, repo, why, "") {
        Reaped::Done => Ok(()),
        Reaped::Queued => Err("certified-queued".into()),
        Reaped::Failed(m) if m.trim().is_empty() => Err("unknown".into()),
        Reaped::Failed(m) => Err(m.trim().to_string()),
    }
}

/// The unit backend for this platform: the user manager over D-Bus on Linux, a plain child
/// process elsewhere.
pub fn default_units() -> Result<Box<dyn Units>, String> {
    let clock: Arc<dyn runtime::ports::Clock> = Arc::new(runtime::ports::WallClock);
    #[cfg(target_os = "linux")]
    {
        runtime::units::SystemdUnits::connect(clock).map(|u| Box::new(u) as Box<dyn Units>).map_err(|e| e.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Box::new(runtime::units::ProcessUnits::new(clock)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::units::ScriptedUnits;

    const UNIT: &str = "spira-dwarf-andvari-sp-a-1.service";

    fn occupied(t: &testkit::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let (proc, run) = (t.join("proc"), t.join("run"));
        let work = run.join("worktree/sp-a");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(proc.join("100")).unwrap();
        std::os::unix::fs::symlink(&work, proc.join("100/cwd")).unwrap();
        std::fs::write(proc.join("100/cgroup"), format!("0::/user.slice/user@1000.service/app.slice/{UNIT}\n")).unwrap();
        (proc, run)
    }

    #[test]
    fn stop_holder_stops_the_dwarf_unit_through_the_units_port() {
        let t = testkit::TempDir::new("hel-verbs-stop-holder");
        let (proc, run) = occupied(&t);
        let units = ScriptedUnits::new(Arc::new(VirtualClock::new(0)));
        let p = proc.clone();
        let r = stop_holder_in(&proc, &run, "sp-a", &units, 3, &|| {
            let _ = std::fs::remove_dir_all(p.join("100"));
        });
        assert_eq!(r, Ok(()));
        assert_eq!(units.calls(), vec![format!("stop {UNIT}")]);
    }

    #[test]
    fn stop_holder_names_the_process_that_will_not_leave() {
        let t = testkit::TempDir::new("hel-verbs-stop-holder-stays");
        let (proc, run) = occupied(&t);
        let units = ScriptedUnits::new(Arc::new(VirtualClock::new(0)));
        let e = stop_holder_in(&proc, &run, "sp-a", &units, 2, &|| {}).unwrap_err();
        assert!(e.contains("[100]"), "{e}");
        assert_eq!(units.calls(), vec![format!("stop {UNIT}")]);
    }
}
