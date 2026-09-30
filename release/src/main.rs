//! `release` — build, verify, activate, rollback and prune spira-releases/<sha>. DESIGN.md.

use release::activate::{self, Ctx};
use release::build::{self, BuildOpts, RealCargo};
use release::config::{self, Config, Flags};
use release::git::RealGit;
use release::systemctl::RealSystemctl;
use release::verify::{self, VerifyOpts};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage:
  release build <commit> [--repo R] [--target-dir T | --bin-dir D]
  release verify <sha> [--no-pre-activate]
  release activate <sha> [--hotfix <reason>] [--repo R] [--landed-ref REF] [--settle SECS]
  release rollback [--repo R] [--settle SECS]
  release prune [--keep N]
  release status
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

fn run(argv: &[String]) -> Result<(), (u8, String)> {
    let usage = |m: String| (2u8, if m.is_empty() { USAGE.to_string() } else { format!("{m}\n{USAGE}") });
    let a = parse(argv).map_err(usage)?;
    let (cmd, rest) = a.pos.split_first().ok_or_else(|| usage(String::new()))?;
    let want = |n: usize| if rest.len() == n { Ok(()) } else { Err(usage(format!("{cmd} takes {n} argument(s)"))) };
    let env = config::process_env();
    let cfg = Config::resolve(&a.flags, &env).map_err(|e| (1, e))?;
    let repo = || a.repo.clone().or_else(|| env.get("SPIRA_REPO").filter(|s| !s.is_empty()).map(PathBuf::from));
    let fail = |e: String| (1u8, e);
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
        other => return Err(usage(format!("unknown subcommand {other}"))),
    }
    Ok(())
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match run(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("release: {msg}");
            ExitCode::from(code)
        }
    }
}
