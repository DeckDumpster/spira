//! `round-controller` — the round controller's binary. Inert: no unit runs it; the world sim's
//! `round-controller` actor does.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use round_controller::outcome::StdoutLog;
use round_controller::pass::pass;
use round_controller::ports::Ports;
use round_controller::real::verbs::{Deadlines, RealVerbs};
use round_controller::real::{self, Unserved};
use round_controller::telemetry;
use runtime::ports::{Proc, SeededRng, SysProc};

const USAGE: &str = "usage: round-controller <verb>

  pass      visit every registered repo once and run the controller's steps
  version   print the version
  -h, --help";

/// A transition verb takes the queue lock for its own transition only, but may merge, build a worktree or push.
const VERB_DEADLINE: Duration = Duration::from_secs(120); // batch-job: a queue round verb merges, builds a worktree or pushes

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = telemetry::run(&telemetry::sink(), args.first().map(String::as_str), || {
        dispatch(&args)
    });
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn dispatch(args: &[String]) -> i32 {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["-h"] | ["--help"] => {
            println!("{USAGE}");
            0
        }
        ["version"] => {
            println!("round-controller {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["pass"] => match run_pass() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("round-controller pass: {e}");
                2
            }
        },
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn run_pass() -> Result<(), String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let repos = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home).all();
    let certify_wall: u64 = spira_config::process::cfg("SPIRA_ROUND_CERTIFY_WALL_SECS")?
        .parse()
        .map_err(|e| format!("SPIRA_ROUND_CERTIFY_WALL_SECS: {e}"))?;
    let clock = real::clock(spira_config::vtime::override_epoch());
    // The simulator pins the clock through SPIRA_NOW, and the stream with it.
    let rng = Mutex::new(SeededRng::new(clock.now_ms()));
    let proc_: Arc<dyn Proc> = Arc::new(SysProc);
    let verbs = RealVerbs::new(
        proc_.clone(),
        "queue",
        Deadlines {
            verb: VERB_DEADLINE,
            certify: VERB_DEADLINE + Duration::from_secs(certify_wall),
        },
    );
    let ports = Ports {
        clock: &*clock,
        proc_: &*proc_,
        rng: &rng,
        store: &Unserved,
        verbs: &verbs,
        log: &StdoutLog,
    };
    pass(&ports, &repos);
    Ok(())
}
