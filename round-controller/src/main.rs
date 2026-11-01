//! `round-controller` — the round controller's binary. Inert: no unit runs it; the world sim's
//! `round-controller` actor does.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use round_controller::liveness::LeaseSettings;
use round_controller::outcome::StdoutLog;
use round_controller::pass::pass;
use round_controller::ports::Ports;
use round_controller::probe::{probe, ThreadSleep};
use round_controller::real::liveness::{NoLauncher, NoVmRecords, SystemdUnits};
use round_controller::real::verbs::{Deadlines, RealVerbs};
use round_controller::real::{self, Unserved};
use round_controller::telemetry;
use runtime::ports::{Proc, SeededRng, SysProc};

const USAGE: &str = "usage: round-controller <verb>

  pass                                  visit every registered repo once and run the controller's steps
  lease probe --secs <n> --out <file>   renew a lease of its own for n seconds and write each renewal's lateness
  version                               print the version
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
    let report = |who: &str, r: Result<(), String>| match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("round-controller {who}: {e}");
            2
        }
    };
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
        ["pass"] => report(
            "pass",
            with_ports(|p| {
                let home = spira_config::resolve::locate_home_for_process()?;
                let repos =
                    spira_config::repos::Registry::from_env(std::env::vars().collect(), &home)
                        .all();
                pass(p, &repos);
                Ok(())
            }),
        ),
        ["lease", "probe", "--secs", secs, "--out", out] => {
            report("lease probe", run_probe(secs, out))
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn run_probe(secs: &str, out: &str) -> Result<(), String> {
    let secs: u64 = secs.parse().map_err(|e| format!("--secs {secs:?}: {e}"))?;
    with_ports(|p| {
        let l = probe(p, &ThreadSleep, secs)?;
        std::fs::write(out, l.report()).map_err(|e| format!("{out}: {e}"))
    })
}

fn with_ports(f: impl FnOnce(&Ports) -> Result<(), String>) -> Result<(), String> {
    let certify_wall: u64 = spira_config::process::cfg("SPIRA_ROUND_CERTIFY_WALL_SECS")?
        .parse()
        .map_err(|e| format!("SPIRA_ROUND_CERTIFY_WALL_SECS: {e}"))?;
    let leases = LeaseSettings::from_cfg(&spira_config::process::cfg)?;
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
    let units = SystemdUnits { proc_: &*proc_ };
    let ports = Ports {
        clock: &*clock,
        proc_: &*proc_,
        rng: &rng,
        store: &Unserved,
        verbs: &verbs,
        log: &StdoutLog,
        units: &units,
        vms: &NoVmRecords,
        launcher: &NoLauncher,
        leases: &leases,
    };
    f(&ports)
}
