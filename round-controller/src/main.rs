//! `round-controller` — the round controller's binary. `spira-sift.service` runs its `sifts`
//! verb; every other verb runs only under the world sim's `round-controller` actor.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use round_controller::cut;
use round_controller::ledger::{FaultPolicy, StoreLedger};
use round_controller::ledger_verbs;
use round_controller::cutover_check;
use round_controller::real::facts::RealFacts;
use round_controller::liveness::LeaseSettings;
use round_controller::outcome::StdoutLog;
use round_controller::pass::pass;
use round_controller::ports::{Launched, Ports};
use round_controller::probe::{probe, ThreadSleep};
use round_controller::real;
use round_controller::real::child::SysSpawn;
use round_controller::real::git::RealGit;
use round_controller::real::installed::InstalledRelease;
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
use runtime::ports::{Proc, ProcSpec, SeededRng, SysProc};

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
  ledger record --repo <r> --batch <b> --members <id@tip,...> --base <sha> --image-digest <d> --harness-version <v> --selection <s> --class <green|flaky|red:<step>|fault:<cause>> [--evidence <text>]
                                            record one attempt on that identity and print the summary the ledger kept
  ledger refuses --repo <r> --members <id@tip,...> --base <sha> --image-digest <d> --harness-version <v> --selection <s>
                                            print whether the Planner may open that identity, and why not
  lane record                               write each registered repo's lanes/<repo> from git and the installed release, and nothing else
  cutover-check --readiness                 one row per round repo (ci, base, submitted) and the world's; exit 0 only when all are green
  cutover-check --since-rev <rev>           each round repo's first round landed after store revision rev, or none; exit 0 only when none reads none
  cutover-check --probe-base <repo>         the base run on the landing ref at the lane's image, under a hand slot, recorded as the base identity's attempt
  version                                   print the version
  -h, --help";

/// A worktree of the landing ref is checked out for the probe; git's work scales with the tree.
const PROBE_GIT_DEADLINE: Duration = Duration::from_secs(600); // batch-job: a worktree checkout scales with the repository

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
        ["ledger", verb @ ("record" | "refuses"), rest @ ..] => {
            let answer = match *verb {
                "record" => ledger_verbs::parse_record(rest)
                    .and_then(|r| with_ports(|p| ledger_verbs::record(p, &r))),
                _ => ledger_verbs::parse_refuses(rest)
                    .and_then(|id| with_ports(|p| ledger_verbs::refuses(p, &id))),
            };
            match answer {
                Ok(text) => {
                    println!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("round-controller ledger {verb}: {e}");
                    2
                }
            }
        }
        ["lane", "record"] => match with_ports(|p| {
            let home = spira_config::resolve::locate_home_for_process()?;
            let repos =
                spira_config::repos::Registry::from_env(std::env::vars().collect(), &home).all();
            Ok(round_controller::lane::record_all(p, &repos))
        }) {
            Ok((out, ok)) => {
                print!("{out}");
                i32::from(!ok)
            }
            Err(e) => {
                eprintln!("round-controller lane record: {e}");
                2
            }
        },
        ["cutover-check", rest @ ..] => match cutover_check::parse(rest) {
            Err(e) => {
                eprintln!("round-controller cutover-check: {e}");
                2
            }
            Ok(mode) => match with_ports(|p| cutover(p, &mode)) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("round-controller cutover-check: {e}");
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

fn sifts_verb() -> Result<(), String> {
    spira_config::release_env::ensure_release_path()?;
    round_controller::sifts::for_process(&mut std::io::stdout().lock())
}

fn cutover(p: &Ports, mode: &cutover_check::Mode) -> Result<i32, String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let registry = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    let located = registry.clone();
    let facts = RealFacts {
        proc_: Arc::new(SysProc),
        repos: round_controller::real::facts::round_repos(&registry),
        locate: Box::new(move |repo| {
            Some((
                located.root(repo)?,
                spira_config::repos::landref(&located, repo)?,
            ))
        }),
        views: spira_config::lc_call::LcClient::for_bin(&spira_config::lifecycle_row::lc_bin()?)
            .and_then(store_view::Projection::configured),
        queue: match spira_config::process::cfg_parse::<u8>("SPIRA_LC_LIFECYCLE_ON_STORE")? {
            0 => Some(spira_config::release_env::tool("queue")?),
            _ => None,
        },
        spira_lc: spira_config::release_env::tool("spira-lc")?,
    };
    match mode {
        cutover_check::Mode::Readiness => {
            let r = cutover_check::readiness(p, &facts);
            print!("{}", r.render());
            Ok(r.code())
        }
        cutover_check::Mode::SinceRev(rev) => {
            let (out, err, code) = cutover_check::render_since(&cutover_check::since_rev(p, &facts, *rev)?);
            print!("{out}");
            eprint!("{err}");
            Ok(code)
        }
        cutover_check::Mode::ProbeBase(repo) => {
            let probed = probe_base(p, &registry, &facts, repo)?;
            print!("{}", probed.render());
            Ok(probed.code())
        }
    }
}

/// The base probe on a detached worktree of the landing ref, the round VM serving its one job.
fn probe_base(
    p: &Ports,
    registry: &spira_config::repos::Registry,
    facts: &RealFacts,
    repo: &str,
) -> Result<cutover_check::Probed, String> {
    let lane = cutover_check::probe_target(p, repo)?;
    let suites = cutover_check::probe_suites(facts, repo, &lane.selection)?;
    // Its own name, so a second probe never shares a tree, a spool or a scope with one whose VM lives.
    let round = format!("base-probe-{:016x}", p.next_u64());
    let cfg = spira_config::process::cfg;
    let num = |k: &str| -> Result<u64, String> {
        cfg(k)?.trim().parse().map_err(|e| format!("{k}: {e}"))
    };
    let run_dir = spira_config::resolve::run_dir_for_process()?;
    let root = registry
        .root(repo)
        .ok_or_else(|| format!("{repo} has no checkout in the registry"))?;
    let wt = run_dir.join("worktree").join(format!(".round-controller-{round}-{repo}"));
    let git = |args: &[&str]| -> Result<(), String> {
        let mut all = vec!["-C".to_string(), root.clone()];
        all.extend(args.iter().map(|a| a.to_string()));
        let o = SysProc.run(&ProcSpec {
            prog: "git".into(),
            args: all,
            stdin: vec![],
            deadline: PROBE_GIT_DEADLINE,
        })?;
        if o.code == Some(0) {
            Ok(())
        } else {
            Err(format!(
                "git {} exited {:?}: {}",
                args.join(" "),
                o.code,
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
    };
    let at = wt.display().to_string();
    let _ = git(&["worktree", "prune"]);
    git(&["worktree", "add", "-q", "--detach", &at, &lane.landing_ref])?;
    let spawn = SysSpawn {
        proc_: Arc::new(SysProc),
    };
    let env = VmEnv {
        round_vm: spira_config::release_env::tool("round-vm")?.into(),
        run: run_dir.clone(),
        wall_secs: num("SPIRA_BATCHER_WALL_SECS")?,
        maxpar: u32::try_from(num("SPIRA_BATCH_MAXPAR")?).map_err(|e| e.to_string())?,
        cpu_quota: std::env::var("ROUND_CPU_QUOTA").ok(),
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
    };
    let vm_repo = Repo {
        name: repo.into(),
        path: root.clone().into(),
        base: registry.base(repo).unwrap_or_default(),
    };
    let mut subs = VmRunner::new(p, &spawn, &ThreadSleep, env, vm_repo, &wt, &lane.landing_ref, &round)?;
    let renv = RunEnv {
        sleep: &ThreadSleep,
        poll_ms: run::POLL_MS,
        teardown_ms: run::TEARDOWN_MS,
        kill_grace_ms: run::KILL_GRACE_MS,
    };
    let unit = format!("round-controller-subpass-{repo}-{round}.scope");
    let probed = cutover_check::probe_base(p, &renv, &mut subs, (repo, &lane), &suites, &unit);
    drop(subs);
    if !matches!(&probed, Ok(pr) if pr.slot_kept) {
        let _ = git(&["worktree", "remove", "-f", &at]);
    }
    probed
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
        round_vm: spira_config::release_env::tool("round-vm")?.into(),
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
        exe: spira_config::release_env::tool("round-controller")?,
        toml: spira_config::process::spec()?,
        release: std::env::var(spira_config::RELEASE_ENV)
            .ok()
            .filter(|r| !r.is_empty()),
    };
    let registry = spira_config::resolve::locate_home_for_process()
        .ok()
        .map(|home| spira_config::repos::Registry::from_env(std::env::vars().collect(), &home));
    let queued = registry.clone();
    let installed = InstalledRelease::new(
        proc_.clone(),
        spira_config::release_env::tool("round-vm")?,
        std::env::var(spira_config::RELEASE_ENV)
            .ok()
            .filter(|r| !r.is_empty())
            .map(Into::into),
        Box::new(move |repo| queued.as_ref().is_some_and(|reg| reg.land_queued(repo))),
    );
    let git = RealGit::new(
        proc_.clone(),
        Box::new(move |repo| {
            let reg = registry.as_ref()?;
            Some((reg.root(repo)?, spira_config::repos::landref(reg, repo)?))
        }),
    );
    let door = spira_config::lc_call::LcClient::for_bin(&spira_config::lifecycle_row::lc_bin()?)?;
    let lc = SpiraLc { client: door.clone() };
    let store = LcStore { client: door };
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
        installed: &installed,
    };
    f(&ports)
}
