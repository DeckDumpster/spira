use gate_worker::{lock_wait, Branches, Clock, Gate, Worker};
use landing_pass::gateq::GateQueue;
use landing_pass::model::RepoRow;
use landing_pass::ports::{Git, Tools};
use landing_pass::real::{load_context, RealGit, RealTools};
use landing_pass::report::Reporter;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
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
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() != Some("run") {
        eprintln!("usage: gate-worker run");
        return ExitCode::from(2);
    }
    let Some(home) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from) else {
        eprintln!("gate-worker: SPIRA_HOME is unset");
        return ExitCode::FAILURE;
    };
    let out = Reporter::stdout(None);
    let (s, repos) = match load_context(&home, &out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("gate-worker: {e}");
            return ExitCode::FAILURE;
        }
    };
    let lock = s.run.join("gate-worker").join("worker.lock");
    let _ = std::fs::create_dir_all(s.run.join("gate-worker"));
    let f = match OpenOptions::new().create(true).write(true).truncate(false).open(&lock) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gate-worker: open {}: {e}", lock.display());
            return ExitCode::FAILURE;
        }
    };
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        println!("gate-worker: another worker is draining the queue — nothing to do");
        return ExitCode::SUCCESS;
    }
    let tools = RealTools::new(s.home.clone(), s.queue_bin.clone(), None, Some(s.run.join("gate-admission")));
    let queue = GateQueue::new(&s.run);
    let gate = RealGate(&tools);
    let branches = Repos(&repos);
    let log = |m: &str| println!("{m}");
    let w = Worker {
        queue: &queue,
        gate: &gate,
        branches: &branches,
        clock: &Wall,
        lock_wait: lock_wait(std::env::var("SPIRA_GATE_TIMEOUT").ok().as_deref(), s.gate_lock_wait.as_deref()),
        log: &log,
    };
    let n = w.drain();
    println!("gate-worker: {n} verdict(s) filed");
    ExitCode::SUCCESS
}
