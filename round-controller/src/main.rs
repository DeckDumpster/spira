//! `round-controller` — the round controller's binary. `spira-sift.service` runs its `sifts`
//! verb; every other verb runs only under the world sim's `round-controller` actor.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use round_controller::cut;
use round_controller::ledger::{FaultPolicy, StoreLedger};
use round_controller::liveness::LeaseSettings;
use round_controller::outcome::StdoutLog;
use round_controller::pass::pass;
use round_controller::ports::{Launched, Ports};
use round_controller::probe::{probe, ThreadSleep};
use round_controller::real;
use round_controller::real::child::SysSpawn;
use round_controller::real::git::RealGit;
use round_controller::real::launch::{Programs, Systemd, SystemdUnits};
use round_controller::real::lc::SpiraLc;
use round_controller::real::liveness::NoVmRecords;
use round_controller::real::store::LcStore;
use round_controller::real::verbs::{Deadlines, RealVerbs};
use round_controller::real::vm_runner::{Repo, VmEnv, VmRunner};
use round_controller::resources::RoundView;
use round_controller::run::{self, RunEnv};
use round_controller::switches::RunDir;
use round_controller::slots::{self, SlotGrant, SlotSettings};
use round_controller::telemetry;
use runtime::ports::{Proc, SeededRng, SysProc};

const USAGE: &str = "usage: round-controller <verb>

  pass                                      visit every registered repo once and run the controller's steps
  sifts                                     screen every queue repo's SUBMITTED and CERTIFIED beads and record a verdict per tip
  cut --repo <r> --members <id@tip,...>     open one round of exactly these members, refused through the ledger
  run --repo <r> --batch <b> --lease-token <t> --slot <pool>/<n> --slot-token <s>
                                            one pass and its Judge under the round lease and the slot pass handed over
  launch --repo <r> --batch <b> --lease-token <t> --slot <pool>/<n> --slot-token <s>
                                            start that run as the transient unit round-controller-run-<batch>-<lease-token>
  lease clear --repo <r> --batch <b>        release a round's lease once its holder is proven dead; refused otherwise
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
        ["launch", rest @ ..] => match run::parse_args(rest) {
            Err(e) => {
                eprintln!("round-controller launch: {e}\n{USAGE}");
                2
            }
            // A hand launch records no unit under the slot; the runner records its own first.
            Ok(a) => match with_ports(|p| {
                let slot = SlotGrant::handed(p, a.slot, a.slot_token);
                Ok(p.launcher.start(p, &a.repo, &a.batch, a.lease, &slot))
            }) {
                Ok(Launched::Started { unit }) => {
                    println!("unit={unit}\nlaunched=started");
                    0
                }
                Ok(Launched::Running { unit }) => {
                    println!("unit={unit}\nlaunched=running");
                    0
                }
                Ok(Launched::Failed { text }) => {
                    eprintln!("round-controller launch: {}", text.trim_end());
                    1
                }
                Err(e) => {
                    eprintln!("round-controller launch: {e}");
                    2
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
        ["sifts"] => match sifts_verb() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("round-controller sifts: {e}");
                1
            }
        },
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
        ["lease", "clear", "--repo", repo, "--batch", batch] => {
            match with_ports(|p| round_controller::clear::clear(p, repo, batch)) {
                Ok(round_controller::clear::Cleared::Unheld) => {
                    println!("lease=none batch={batch}");
                    0
                }
                Ok(round_controller::clear::Cleared::Released {
                    holder,
                    token,
                    evidence,
                }) => {
                    println!("lease=cleared batch={batch} holder={holder} token={token}\nevidence={evidence}");
                    0
                }
                Err(e) => {
                    eprintln!("round-controller lease clear: {e}");
                    1
                }
            }
        }
        ["lease", "probe", "--secs", secs, "--out", out] => {
            report("lease probe", run_probe(secs, out))
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn sifts_verb() -> Result<(), String> {
    spira_config::release_env::ensure_release_path();
    round_controller::sifts::for_process(&mut std::io::stdout().lock())
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
        run: run_dir,
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
    let renv = RunEnv {
        sleep: &ThreadSleep,
        poll_ms: run::POLL_MS,
        teardown_ms: run::TEARDOWN_MS,
        kill_grace_ms: run::KILL_GRACE_MS,
    };
    Ok(run::run(p, &renv, &mut subs, a))
}

fn with_ports<T>(f: impl FnOnce(&Ports) -> Result<T, String>) -> Result<T, String> {
    let leases = LeaseSettings::from_cfg(&spira_config::process::cfg)?;
    // The off switches live in the run directory: a pass that cannot find it cannot obey them.
    let stops = RunDir(
        spira_config::resolve::run_dir_for_process()
            .map_err(|e| format!("the run directory, where the off switches live: {e}"))?,
    );
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
    let programs = Programs::from_env(&|k| std::env::var(k).ok());
    let units = SystemdUnits {
        proc_: &*proc_,
        ctl: programs.ctl.clone(),
    };
    let launcher = Systemd {
        programs,
        exe: spira_config::release_env::tool("round-controller"),
        toml: spira_config::process::spec()?,
        release: std::env::var(spira_config::RELEASE_ENV)
            .ok()
            .filter(|r| !r.is_empty()),
    };
    let registry = spira_config::resolve::locate_home_for_process()
        .ok()
        .map(|home| spira_config::repos::Registry::from_env(std::env::vars().collect(), &home));
    let git = RealGit::new(
        proc_.clone(),
        Box::new(move |repo| {
            let reg = registry.as_ref()?;
            Some((reg.root(repo)?, spira_config::repos::landref(reg, repo)?))
        }),
    );
    let lc = SpiraLc {
        proc_: proc_.clone(),
        program: spira_config::release_env::tool("spira-lc"),
    };
    let store = LcStore {
        proc_: proc_.clone(),
        program: spira_config::release_env::tool("spira-lc"),
    };
    let ledger = StoreLedger {
        policy: FaultPolicy::from_config()?,
    };
    let sink = telemetry::sink();
    let log = telemetry::DwellLog {
        sink: &sink,
        inner: &StdoutLog,
    };
    let ports = Ports {
        clock: &*clock,
        proc_: &*proc_,
        rng: &rng,
        store: &store,
        verbs: &verbs,
        ledger: &ledger,
        log: &log,
        units: &units,
        vms: &NoVmRecords,
        launcher: &launcher,
        git: &git,
        leases: &leases,
        slots: &slot_settings,
        lc: &lc,
        stops: &stops,
    };
    f(&ports)
}
