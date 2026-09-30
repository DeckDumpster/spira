//! `release` — build, verify, activate, rollback and prune spira-releases/<sha>. DESIGN.md.

use release::activate::{self, Ctx};
use release::build::{self, BuildOpts, RealCargo};
use release::canary::{self, CanaryOpts};
use release::config::{self, Config, Flags};
use release::git::RealGit;
use release::install::{self, InstallOpts, RealUnpack};
use release::stage::{self, StageOpts};
use release::systemctl::RealSystemctl;
use release::verify::{self, VerifyOpts};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage:
  release build <commit> [--repo R] [--target-dir T | --bin-dir D]
  release verify <sha> [--no-pre-activate]
  release activate <sha> [--hotfix <reason>] [--repo R] [--landed-ref REF] [--settle SECS]
  release rollback [--repo R] [--settle SECS]
  release prune [--keep N]
  release status
  release install-tarball <tarball> [--dry-run] [--settle SECS]
  release stage up [ROOT]
  release stage down <ROOT>
  release canary [--stage ROOT] [--deadline SECS]
  release canary-worker
  release acceptance <tag> --scratch-repo <path> [...]   (release acceptance --help)
every subcommand also takes --releases D and --run D";

struct Args {
    pos: Vec<String>,
    flags: Flags,
    repo: Option<PathBuf>,
    target_dir: Option<PathBuf>,
    bin_dir: Option<PathBuf>,
    hotfix: Option<String>,
    landed_ref: String,
    settle: Duration,
    pre_activate: bool,
    dry_run: bool,
    stage: Option<String>,
    deadline: Duration,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        pos: Vec::new(),
        flags: Flags::default(),
        repo: None,
        target_dir: None,
        bin_dir: None,
        hotfix: None,
        landed_ref: "local/main".into(),
        settle: Duration::from_secs(3),
        pre_activate: true,
        dry_run: false,
        stage: None,
        deadline: Duration::from_secs(120),
    };
    let mut it = argv.iter();
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match x.as_str() {
            "--releases" => a.flags.releases = Some(val(x)?.into()),
            "--run" => a.flags.run = Some(val(x)?.into()),
            "--keep" => a.flags.keep = Some(val(x)?.parse().map_err(|_| "--keep needs a whole number".to_string())?),
            "--repo" => a.repo = Some(val(x)?.into()),
            "--target-dir" => a.target_dir = Some(val(x)?.into()),
            "--bin-dir" => a.bin_dir = Some(val(x)?.into()),
            "--hotfix" => a.hotfix = Some(val(x)?),
            "--landed-ref" => a.landed_ref = val(x)?,
            "--settle" => a.settle = Duration::from_secs(val(x)?.parse().map_err(|_| "--settle needs whole seconds".to_string())?),
            "--no-pre-activate" => a.pre_activate = false,
            "--dry-run" => a.dry_run = true,
            "--stage" => a.stage = Some(val(x)?),
            "--deadline" => a.deadline = Duration::from_secs(val(x)?.parse().map_err(|_| "--deadline needs whole seconds".to_string())?),
            "-h" | "--help" => return Err(String::new()),
            f if f.starts_with('-') => return Err(format!("unknown flag {f}")),
            p => a.pos.push(p.to_string()),
        }
    }
    Ok(a)
}

fn system_dirs() -> Vec<PathBuf> {
    release::SYSTEM_DIRS.iter().map(PathBuf::from).collect()
}

fn which(prog: &str, path: Option<&String>) -> Option<PathBuf> {
    let path = path.cloned().or_else(|| std::env::var("PATH").ok())?;
    path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(prog)).find(|p| p.is_file())
}

/// `stage up`'s roots: `SPIRA_RELEASE`'s own `spira/` (the release's tools, never a
/// checkout — DESIGN.md "stage") and `bd-embedded` resolved from `PATH`. `TESTDB_SHARED`
/// together with `TESTDB_BASELINE`, when both are set, is stage.sh's own fast path.
/// `SPIRA_SCOPE_LABEL` is deliberately never read from the caller's environment: a stage
/// always starts with it empty (DESIGN.md "stage").
fn resolve_stage_opts(env: &config::Env, root: Option<PathBuf>) -> Result<StageOpts, String> {
    let release = env.get("SPIRA_RELEASE").filter(|s| !s.is_empty()).ok_or("SPIRA_RELEASE is not set — stage needs it to find the release's own spira/ scripts")?;
    let harness_spira = PathBuf::from(release).join("spira");
    let bd_embedded = which("bd-embedded", env.get("PATH")).ok_or("bd-embedded not found; install: npm install -g @beads/bd")?;
    let testdb_baseline = if env.get("TESTDB_SHARED").map(String::as_str) == Some("1") { env.get("TESTDB_BASELINE").filter(|s| !s.is_empty()).map(PathBuf::from) } else { None };
    Ok(StageOpts { root, harness_spira, bd_embedded, testdb_baseline, scope_label: String::new() })
}

fn run(argv: &[String]) -> Result<(), (u8, String)> {
    let usage = |m: String| (2u8, if m.is_empty() { USAGE.to_string() } else { format!("{m}\n{USAGE}") });
    let a = parse(argv).map_err(usage)?;
    let (cmd, rest) = a.pos.split_first().ok_or_else(|| usage(String::new()))?;
    let want = |n: usize| if rest.len() == n { Ok(()) } else { Err(usage(format!("{cmd} takes {n} argument(s)"))) };
    let env = config::process_env();
    let fail = |e: String| (1u8, e);

    // stage/canary/canary-worker operate entirely under their own STAGE_ROOT (or, for
    // canary-worker, a stage's inherited env) and never touch spira-releases/ or the host
    // config document — resolving Config here would make canary-worker (run inside an
    // isolated stage that deliberately sets no SPIRA_RELEASES, DESIGN.md "stage": isolation)
    // fail before it ever got to its own work, or worse, read the real host's own config.
    match cmd.as_str() {
        "stage" => {
            let (sub, srest) = rest.split_first().ok_or_else(|| usage("stage needs a subcommand: up or down".into()))?;
            match sub.as_str() {
                "up" => {
                    if srest.len() > 1 {
                        return Err(usage("stage up takes at most one argument (ROOT)".into()));
                    }
                    let so = resolve_stage_opts(&env, srest.first().map(PathBuf::from)).map_err(fail)?;
                    let s = stage::up(&so).map_err(fail)?;
                    for (k, v) in &s.env {
                        println!("export {k}={v}");
                    }
                }
                "down" => {
                    let root = srest.first().ok_or_else(|| usage("stage down needs <ROOT>".into()))?;
                    stage::down(Path::new(root)).map_err(fail)?;
                }
                other => return Err(usage(format!("unknown stage subcommand {other}"))),
            }
            return Ok(());
        }
        "canary" => {
            want(0)?;
            let external_stage = a.stage.as_ref().map(PathBuf::from);
            // Only needed to stand up canary's OWN stage; with --stage it is never read.
            let stage_opts = match &external_stage {
                Some(_) => StageOpts { root: None, harness_spira: PathBuf::new(), bd_embedded: PathBuf::new(), testdb_baseline: None, scope_label: String::new() },
                None => resolve_stage_opts(&env, None).map_err(fail)?,
            };
            let verdict_window = env.get("SPIRA_VERDICT_WINDOW").and_then(|s| s.parse().ok()).unwrap_or(400);
            let o = CanaryOpts { external_stage, stage_opts, deadline: a.deadline, verdict_window };
            let r = canary::canary(&o).map_err(fail)?;
            println!("release: canary PASS — commit '{}' on origin/main in {}s (stage {})", r.commit, r.elapsed.as_secs(), r.stage_root.display());
            return Ok(());
        }
        "canary-worker" => {
            want(0)?;
            canary::canary_worker().map_err(fail)?;
            return Ok(());
        }
        _ => {}
    }

    let cfg = Config::resolve(&a.flags, &env).map_err(|e| (1, e))?;
    let repo = || a.repo.clone().or_else(|| env.get("SPIRA_REPO").filter(|s| !s.is_empty()).map(PathBuf::from));
    match cmd.as_str() {
        "build" => {
            want(1)?;
            let r = repo().unwrap_or_else(|| PathBuf::from("."));
            if a.bin_dir.is_some() && a.target_dir.is_some() {
                return Err(usage("--bin-dir and --target-dir exclude each other (--bin-dir runs no cargo)".into()));
            }
            let o = BuildOpts { repo: &r, commit: &rest[0], target_dir: a.target_dir.clone(), system_dirs: system_dirs(), bin_dir: a.bin_dir.clone() };
            let b = build::build(&cfg, &RealGit, &RealCargo, &o).map_err(fail)?;
            println!("{}", b.sha);
        }
        "verify" => {
            want(1)?;
            let p = verify::verify(&cfg, &rest[0], &VerifyOpts { pre_activate: a.pre_activate, system_dirs: system_dirs() }).map_err(fail)?;
            if !p.is_empty() {
                return Err((1, format!("release {} FAILS verify:\n  {}", rest[0], p.join("\n  "))));
            }
            println!("release {} verifies", rest[0]);
        }
        "activate" | "rollback" => {
            let sc = RealSystemctl::from_env();
            let ctx = Ctx { cfg: &cfg, sc: &sc, git: &RealGit, repo: repo(), landed_ref: a.landed_ref.clone(), settle: a.settle };
            if cmd == "activate" {
                want(1)?;
                let s = activate::activate(&ctx, &rest[0], a.hotfix.as_deref()).map_err(fail)?;
                println!("release: {} active; {} unit file(s) rewritten, restarted [{}], deferred to next start [{}]", rest[0], s.rewritten.len(), s.restarted.join(" "), s.deferred.join(" "));
            } else {
                want(0)?;
                let sha = activate::rollback(&ctx).map_err(fail)?;
                println!("release: rolled back to {sha}");
            }
        }
        "prune" => {
            want(0)?;
            let p = release::prune::prune(&cfg).map_err(fail)?;
            println!("release: prune kept {}, removed {}: {}", p.kept.len(), p.removed.len(), p.removed.join(" "));
            if !p.failed.is_empty() {
                return Err((1, format!("prune could not remove:\n  {}", p.failed.join("\n  "))));
            }
        }
        "status" => {
            want(0)?;
            print!("{}", activate::status(&cfg).map_err(fail)?);
        }
        "install-tarball" => {
            want(1)?;
            let sc = RealSystemctl::from_env();
            let o = InstallOpts { dry_run: a.dry_run, settle: a.settle };
            let r = install::install(&cfg, &sc, &RealUnpack, &PathBuf::from(&rest[0]), &o).map_err(fail)?;
            println!(
                "release: {} {}; {} unit(s) restarted [{}]{}, {} release(s) pruned [{}]",
                r.name,
                if a.dry_run { "would install" } else if r.fresh { "installed" } else { "already present" },
                r.restarted.len(),
                r.restarted.join(" "),
                if r.restart_failed.is_empty() { String::new() } else { format!(" ({} failed)", r.restart_failed.len()) },
                r.pruned.len(),
                r.pruned.join(" "),
            );
        }
        other => return Err(usage(format!("unknown subcommand {other}"))),
    }
    Ok(())
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // acceptance has its own flags (DESIGN.md "acceptance") and, on a clean machine, no
    // config document to resolve — it never reaches the shared parser or Config.
    if argv.first().map(String::as_str) == Some("acceptance") {
        return ExitCode::from(release::acceptance::main(&argv[1..]));
    }
    match run(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("release: {msg}");
            ExitCode::from(code)
        }
    }
}
