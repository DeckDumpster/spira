//! `round-controller` — the round controller's binary. Inert: no unit runs it; the world sim's
//! `round-controller` actor does.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use round_controller::cut;
use round_controller::liveness::LeaseSettings;
use round_controller::outcome::StdoutLog;
use round_controller::pass::pass;
use round_controller::ports::Ports;
use round_controller::probe::{probe, ThreadSleep};
use round_controller::real::child::SysSpawn;
use round_controller::real::liveness::{NoLauncher, NoVmRecords, SystemdUnits};
use round_controller::real::verbs::{Deadlines, RealVerbs};
use round_controller::real::vm_runner::{Repo, VmEnv, VmRunner};
use round_controller::real::{self, Unserved};
use round_controller::resources::RoundView;
use round_controller::run::{self, RunDir, RunEnv};
use round_controller::slots::{self, SlotSettings};
use round_controller::telemetry;
use runtime::ports::{Proc, SeededRng, SysProc};

const USAGE: &str = "usage: round-controller <verb>

  pass                                      visit every registered repo once and run the controller's steps
  cut --repo <r> --members <id@tip,...>     open one round of exactly these members, refused through the ledger
  run --repo <r> --batch <b> --lease-token <t> --slot <pool>/<n> --slot-token <s>
                                            one pass and its Judge under the round lease and the slot pass handed over
  lease probe --secs <n> --out <file>       renew a lease of its own for n seconds and write each renewal's lateness
  version                                   print the version
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
            println!("{USAGE}\n\n{}", run::EXITS);
            0
        }
        ["run", rest @ ..] => match run::parse_args(rest) {
            Err(e) => {
                eprintln!("round-controller run: {e}\n{USAGE}");
                2
            }
            Ok(a) => match with_ports(|p| run_pass(p, &a)) {
                Ok(end) => {
                    println!("run={} code={}", end.word(), end.code());
                    if let run::RunEnd::Refused { why } | run::RunEnd::Stopped { why } = &end {
                        eprintln!("round-controller run: {}: {why}", end.word());
                    }
                    end.code()
                }
                Err(e) => {
                    eprintln!("round-controller run: {e}");
                    run::RunEnd::Refused { why: e }.code()
                }
            },
        },
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
                if let Ok(used) = slots::in_use(p) {
                    telemetry::push_slots(&telemetry::sink(), &used);
                }
                Ok(())
            }),
        ),
        ["cut", rest @ ..] => match cut::parse_explicit(rest) {
            Err(e) => {
                eprintln!("round-controller cut: {e}\n{USAGE}");
                2
            }
            Ok((repo, members)) => match with_ports(|p| Ok(cut::explicit(p, &repo, members))) {
                Ok(Ok(o)) => {
                    println!("batch={}\nhead={}", o.batch, o.head);
                    0
                }
                Ok(Err(r)) => {
                    eprintln!("round-controller cut: refused {r}");
                    1
                }
                Err(e) => {
                    eprintln!("round-controller cut: {e}");
                    2
                }
            },
        },
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

/// The sub-pass VM runs on the round's own tree: its worktree and tested base, from its record.
fn run_pass(p: &Ports, a: &run::RunArgs) -> Result<run::RunEnd, String> {
    let cfg = spira_config::process::cfg;
    let num = |k: &str| -> Result<u64, String> {
        cfg(k)?.trim().parse().map_err(|e| format!("{k}: {e}"))
    };
    let home = spira_config::resolve::locate_home_for_process()?;
    let run_dir = spira_config::resolve::run_dir_for_process()?;
    let registry = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    let tree = p
        .store
        .get(&round_controller::resources::round_key(&a.repo, &a.batch))
        .ok()
        .flatten()
        .and_then(|i| {
            i.value
                .data
                .get("worktree")?
                .as_str()
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_default();
    let base = RoundView::read(p.store, &a.repo, &a.batch)
        .ok()
        .flatten()
        .and_then(|v| v.round.tested_base.or(v.round.row.base))
        .unwrap_or_default();
    let spawn = SysSpawn {
        proc_: Arc::new(SysProc),
    };
    let env = VmEnv {
        round_vm: spira_config::release_env::tool("round-vm").into(),
        run: run_dir.clone(),
        wall_secs: num("SPIRA_BATCHER_WALL_SECS")?,
        maxpar: u32::try_from(num("SPIRA_BATCH_MAXPAR")?).map_err(|e| e.to_string())?,
        cpu_quota: std::env::var("ROUND_CPU_QUOTA").ok(),
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
    };
    let repo = Repo {
        name: a.repo.clone(),
        path: registry.root(&a.repo).unwrap_or_default().into(),
        base: registry.base(&a.repo).unwrap_or_default(),
    };
    let mut subs = VmRunner::new(p, &spawn, &ThreadSleep, env, repo, &tree, &base, &a.batch)?;
    let switches = RunDir(run_dir);
    let renv = RunEnv {
        switches: &switches,
        sleep: &ThreadSleep,
        poll_ms: run::POLL_MS,
        teardown_ms: run::TEARDOWN_MS,
        kill_grace_ms: run::KILL_GRACE_MS,
    };
    Ok(run::run(p, &renv, &mut subs, a))
}

fn with_ports<T>(f: impl FnOnce(&Ports) -> Result<T, String>) -> Result<T, String> {
    let leases = LeaseSettings::from_cfg(&spira_config::process::cfg)?;
    let slot_settings = SlotSettings::from_cfg(&spira_config::process::cfg)?;
    let clock = real::clock(spira_config::vtime::override_epoch());
    // The simulator pins the clock through SPIRA_NOW, and the stream with it.
    let rng = Mutex::new(SeededRng::new(clock.now_ms()));
    let proc_: Arc<dyn Proc> = Arc::new(SysProc);
    let verbs = RealVerbs::new(
        proc_.clone(),
        Arc::new(SysSpawn {
            proc_: proc_.clone(),
        }),
        "queue",
        Deadlines {
            verb: VERB_DEADLINE,
        },
        spira_config::resolve::run_dir_for_process().ok(),
    );
    let units = SystemdUnits { proc_: &*proc_ };
    let ports = Ports {
        clock: &*clock,
        proc_: &*proc_,
        rng: &rng,
        store: &Unserved,
        verbs: &verbs,
        ledger: &Unserved,
        log: &StdoutLog,
        units: &units,
        vms: &NoVmRecords,
        launcher: &NoLauncher,
        leases: &leases,
        slots: &slot_settings,
    };
    f(&ports)
}
