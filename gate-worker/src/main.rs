use gate_worker::{acquire_slot, release_is_current, lock_wait, worker_count, Branches, Clock, Gate, Worker};
use landing_pass::gateq::GateQueue;
use landing_pass::model::RepoRow;
use landing_pass::ports::{Git, Tools};
use landing_pass::real::{load_context, RealGit, RealTools};
use landing_pass::report::Reporter;
use landing_pass::util::command;
use std::path::PathBuf;
use std::process::{Child, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

struct RealGate<'a>(&'a RealTools);
impl Gate for RealGate<'_> {
    fn gate(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> (i32, String) {
        self.0.gate(branch, repo, lock_wait, bead)
    }
}

struct Repos<'a>(&'a [RepoRow]);
impl Branches for Repos<'_> {
    fn tip(&self, repo: &str, branch: &str) -> Option<String> {
        let r = self.0.iter().find(|r| r.name == repo)?;
        RealGit.rev_parse(&r.path, &format!("refs/heads/{branch}"))
    }
}

struct Wall;
impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        match spira_config::vtime::override_epoch() {
            Some(s) => s * 1000,
            None => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("run") {
        eprintln!("usage: gate-worker run [--worker]");
        return ExitCode::from(2);
    }
    // The timer fires this binary once per tick with plain `run`. `--worker` marks a copy
    // this same binary spawned to fill a second slot — it never spawns further copies, so
    // one tick fans out to at most N processes total, never a recursive storm.
    // Certification is speculative work: its gates yield scratch room to a landing round.
    std::env::set_var("SPIRA_GATE_CLASS", "certify");
    let spawned = args.get(2).map(String::as_str) == Some("--worker");
    let Some(home) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from) else {
        eprintln!("gate-worker: SPIRA_HOME is unset");
        return ExitCode::FAILURE;
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| home.clone());
    let out = Reporter::stdout(None);
    let (s, repos) = match load_context(&home, &out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("gate-worker: {e}");
            return ExitCode::FAILURE;
        }
    };
    // N is `certify_par` (DESIGN.md §8 D14) — the one knob that already governs how many
    // gates run at once, never a new env var. The host's own gate-admission slots bound
    // concurrency further; N worker slots just let the queue be drained that fast instead
    // of one job at a time (sp-kbjv6 "wave N concurrent drains").
    let n = worker_count(s.certify_par);
    let lock_dir = s.run.join("gate-worker");
    let _ = std::fs::create_dir_all(&lock_dir);

    // One tick starts enough workers to fill the free slots: the timer-triggered process
    // (never a `--worker` copy) spawns up to N-1 helper copies of itself, each racing the
    // others for whichever slot is still free, and waits for all of them so the systemd
    // unit's lifetime — and its cgroup — covers every worker it started, not just its own.
    let mut children: Vec<Child> = Vec::new();
    if !spawned && n > 1 {
        match std::env::current_exe() {
            Ok(exe) => {
                for _ in 1..n {
                    match command(&exe).arg("run").arg("--worker").spawn() {
                        Ok(c) => children.push(c),
                        Err(e) => eprintln!("gate-worker: could not start a helper worker: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("gate-worker: could not resolve its own path to start helper workers: {e}"),
        }
    }

    let result = match acquire_slot(&lock_dir, n) {
        None => {
            println!("gate-worker: another worker is draining the queue — nothing to do");
            ExitCode::SUCCESS
        }
        Some((slot, _held)) => {
            let tools = RealTools::new(s.home.clone(), s.queue_bin.clone(), None, Some(s.run.join("gate-admission")));
            let queue = GateQueue::new(&s.run);
            let gate = RealGate(&tools);
            let branches = Repos(&repos);
            let log = |m: &str| println!("{m}");
            // SPIRA_GATE_TIMEOUT is a registered key (spira/conf.d) — the one source of
            // config, read once here rather than from the process environment.
            let gate_timeout = match spira_config::process::cfg("SPIRA_GATE_TIMEOUT") {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("gate-worker: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let w = Worker {
                queue: &queue,
                gate: &gate,
                branches: &branches,
                clock: &Wall,
                lock_wait: lock_wait(Some(&gate_timeout), s.gate_lock_wait.as_deref()),
                log: &log,
                slot,
            };
            let filed = w.drain_while(&|| {
                let current = release_is_current(&exe);
                if !current {
                    println!("gate-worker: release {} is no longer current — exiting so the next tick runs the new one", exe.display());
                }
                current
            });
            println!("gate-worker: {filed} verdict(s) filed (slot {slot})");
            ExitCode::SUCCESS
        }
    };

    for mut c in children {
        let _ = c.wait();
    }
    result
}

#[cfg(test)]
mod vtime_tests {
    use super::*;


    #[test]
    fn wall_honours_spira_now_in_ms() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || Wall.now_ms() / 1000);
        assert_eq!(got, 1_900_000_000);
    }
}
