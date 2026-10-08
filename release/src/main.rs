//! `release` — build, verify, activate, rollback and prune spira-releases/<sha>. DESIGN.md.

use release::activate::{self, Ctx};
use release::build::{self, BuildOpts, RealCargo};
use release::canary::{self, CanaryOpts};
use release::config::{self, Config, Flags};
use release::git::RealGit;
use release::install::{self, InstallOpts, RealUnpack};
use release::intake::{self, RealTemplates};
use release::session_hook;
use release::stage::{self, StageOpts};
use release::systemctl::RealSystemctl;
use release::verify::{self, VerifyOpts};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage:
  release build <commit> [--repo R] [--target-dir T | --bin-dir D]
  release verify <sha> [--no-pre-activate]
  release config-stage <tree>   (prints SPIRA_TOML with <tree>'s config delta applied; the layers stay in a scratch dir)
  release activate <sha> [--hotfix <reason>] [--repo R] [--landed-ref REF] [--settle SECS] [--drain-wait SECS]
  release rollback [--repo R] [--settle SECS] [--drain-wait SECS]
  release prune [--keep N]
  release status
  release install-tarball <tarball> [--answers FILE] [--dry-run] [--settle SECS] [--skip-restart]
  release stage up [ROOT]
  release stage down <ROOT>
  release canary [--stage ROOT] [--deadline SECS]
  release canary-worker
  release acceptance <tag> --scratch-repo <path> [...]   (release acceptance --help)
  release session-hook install|status|uninstall|prune <substring>
  release intake install|status|uninstall
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
    drain: Duration,
    pre_activate: bool,
    dry_run: bool,
    stage: Option<String>,
    deadline: Duration,
    skip_restart: bool,
    answers: Option<PathBuf>,
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
        drain: Duration::from_secs(2700), // batch-job: bounded wait for a running gate to finish, defaults to the gate cap
        pre_activate: true,
        dry_run: false,
        stage: None,
        // batch-job: a release restart waits on every unit to come back
        deadline: Duration::from_secs(120),
        skip_restart: false,
        answers: None,
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
            "--drain-wait" => a.drain = Duration::from_secs(val(x)?.parse().map_err(|_| "--drain-wait needs whole seconds".to_string())?),
            "--settle" => a.settle = Duration::from_secs(val(x)?.parse().map_err(|_| "--settle needs whole seconds".to_string())?),
            "--no-pre-activate" => a.pre_activate = false,
            "--dry-run" => a.dry_run = true,
            "--skip-restart" => a.skip_restart = true,
            "--answers" => a.answers = Some(val(x)?.into()),
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

fn usage_err(m: String) -> (u8, String) {
    (2, if m.is_empty() { USAGE.to_string() } else { format!("{m}\n{USAGE}") })
}

fn fail_err(m: String) -> (u8, String) {
    (1, m)
}

/// [`Config::resolve`], except that `verify` and `activate` of a release resolve against the
/// layers with that release's own config delta already applied: this binary is the new
/// release's, its registry requires the keys the delta adds, and `activate` is what writes them.
/// The real spec stays in `env`, so units are still rendered with, and the delta still applied
/// to, the files in force.
fn resolve_config(a: &Args, env: &config::Env, cmd: &str, rest: &[String]) -> Result<Config, String> {
    let staged = match (cmd, rest.first(), env.get("SPIRA_TOML").filter(|s| !s.is_empty())) {
        ("verify" | "activate", Some(sha), Some(spec)) => release::config_delta::staged_for_resolution(spec, a.flags.releases.as_deref(), sha)?,
        _ => None,
    };
    let Some((dir, spec)) = staged else { return Config::resolve(&a.flags, env) };
    std::env::set_var("SPIRA_TOML", &spec);
    let cfg = Config::resolve(&a.flags, env);
    std::env::set_var("SPIRA_TOML", env.get("SPIRA_TOML").map(String::as_str).unwrap_or_default());
    let _ = std::fs::remove_dir_all(dir);
    cfg
}

/// The release root `install`/`status` address the hook/meter through, and its PATH tail.
///
/// PREFERS `<releases>/current` when [`Config`] resolves and that path actually exists — the
/// production shape this bead's whole fix is about (DESIGN.md "session-hook"): a release
/// rotates, `current` never does, so the registered command never needs to change again.
///
/// FALLS BACK TO `$SPIRA_RELEASE` ITSELF, taken as the release root directly with no
/// `current` join, in two cases only: `Config` cannot resolve at all (no host config
/// document, no `SPIRA_RELEASES`), or it resolves but the releases directory has no
/// `current` link yet.
/// This never fires in production, where `current` always exists and every launcher already
/// sets `SPIRA_RELEASE` before calling this — it fires in a single-release environment with
/// no rotation and no `current` concept at all (a test fixture's own synthetic release, or
/// `concierge.sh start` running against one), where addressing "through current" has no
/// meaning to begin with and refusing to register anything would be strictly worse.
fn release_root_for_session_hook(flags: &Flags, env: &config::Env) -> Result<(PathBuf, String), (u8, String)> {
    let live_release = || env.get("SPIRA_RELEASE").filter(|s| !s.is_empty()).map(PathBuf::from);
    match Config::resolve(flags, env) {
        Ok(cfg) => {
            let want = cfg.releases.join(activate::CURRENT);
            let tail = cfg.path_tail().map_err(fail_err)?;
            if want.exists() {
                Ok((want, tail))
            } else if let Some(rel) = live_release() {
                Ok((rel, tail))
            } else {
                Ok((want, tail)) // no fallback available — let install()/status() name the missing hook
            }
        }
        Err(e) => match live_release() {
            Some(rel) => Ok((rel, String::new())),
            None => Err(fail_err(e)),
        },
    }
}

/// `release session-hook install|status|uninstall|prune <substring>` (DESIGN.md
/// "session-hook"). `uninstall`/`prune` touch only the settings file and resolve no
/// [`Config`] at all; `install`/`status` additionally need the release's own `current` and
/// PATH tail, so [`Config::resolve`] is called only on those two branches — never
/// unconditionally, which would refuse an uninstall on a box whose releases directory
/// cannot be found (exactly the state an uninstall may be reached from).
///
/// NOTE (per Ryan 2026-10-05, one source of config): every branch, `uninstall`/`prune`
/// included, now resolves `SPIRA_CLIENT_SETTINGS` through `cfg` before it does anything
/// else, which means `$SPIRA_TOML` must resolve even for an operation this function was
/// built to keep independent of `Config::resolve`'s own (heavier) requirements. If that
/// turns out to be the wrong trade for `uninstall`/`prune` specifically, the fix is to give
/// `SPIRA_CLIENT_SETTINGS` its own narrower resolution path, not to revert this read to a
/// raw environment lookup.
fn session_hook_cmd(env: &config::Env, a: &Args, rest: &[String]) -> Result<(), (u8, String)> {
    let (sub, srest) = rest.split_first().ok_or_else(|| usage_err("session-hook needs a subcommand: install, status, uninstall or prune <substring>".into()))?;
    // SPIRA_CLIENT_SETTINGS is a registered key (spira/conf.d) — its own registered default
    // is already `$HOME/.claude/settings.json`, so no local HOME-derived fallback is needed
    // here any more (per Ryan 2026-10-05: one source of config). `HOME` itself is not
    // registered and is read only inside `cfg`'s own resolution now, not here.
    let settings = spira_config::process::cfg("SPIRA_CLIENT_SETTINGS")
        .map_err(fail_err)
        .and_then(|s| if s.is_empty() { Err(fail_err("SPIRA_CLIENT_SETTINGS resolved empty".into())) } else { Ok(PathBuf::from(s)) })?;
    let mut doc = session_hook::load(&settings).map_err(fail_err)?;
    match sub.as_str() {
        "uninstall" => {
            if !srest.is_empty() {
                return Err(usage_err("session-hook uninstall takes no arguments".into()));
            }
            let changed = session_hook::uninstall(&mut doc);
            session_hook::save(&settings, &doc).map_err(fail_err)?;
            println!("release: session-hook uninstall: {}", changed.join(", "));
            return Ok(());
        }
        "prune" => {
            let needle = srest.first().ok_or_else(|| usage_err("session-hook prune needs a substring".into()))?;
            let changed = session_hook::prune(&mut doc, needle);
            session_hook::save(&settings, &doc).map_err(fail_err)?;
            println!("release: session-hook prune: {}", changed.join(", "));
            return Ok(());
        }
        "install" | "status" => {}
        other => return Err(usage_err(format!("unknown session-hook subcommand {other}"))),
    }

    let (current, tail) = release_root_for_session_hook(&a.flags, env)?;
    let toml = spira_config::process::spec().map_err(fail_err)?;
    let paths = session_hook::resolve(&current, &tail, &toml, settings).map_err(fail_err)?;
    match sub.as_str() {
        "install" => {
            if !srest.is_empty() {
                return Err(usage_err("session-hook install takes no arguments".into()));
            }
            let changed = session_hook::install(&mut doc, &paths).map_err(fail_err)?;
            if session_hook::save(&paths.settings, &doc).map_err(fail_err)? {
                println!("release: session-hook install: {}", changed.join(", "));
            }
        }
        "status" => {
            if !srest.is_empty() {
                return Err(usage_err("session-hook status takes no arguments".into()));
            }
            println!("session hook: {}", paths.hook.display());
            println!("meter:        {}", paths.meter.display());
            println!("settings:     {}", paths.settings.display());
            let (lines, ok) = session_hook::status(&doc, &paths);
            for l in &lines {
                println!("{l}");
            }
            if !ok {
                return Err((1, String::new()));
            }
        }
        _ => unreachable!("filtered above"),
    }
    Ok(())
}

/// `release intake install|status|uninstall` (DESIGN.md "intake"). Needs only the systemd
/// unit directory — [`config::unit_dir_from_env`], never the full [`Config`], which would
/// couple every intake call to the releases directory resolving even though intake has
/// nothing to do with one.
///
/// NOTE (per Ryan 2026-10-05, one source of config): `SPIRA_ALERT_GLOB` below now resolves
/// through `cfg`, so every subcommand still needs `$SPIRA_TOML` to resolve, even though it
/// no longer needs the releases directory specifically to.
fn intake_cmd(env: &config::Env, rest: &[String]) -> Result<(), (u8, String)> {
    let (sub, srest) = rest.split_first().ok_or_else(|| usage_err("intake needs a subcommand: install, status or uninstall".into()))?;
    if !srest.is_empty() {
        return Err(usage_err(format!("intake {sub} takes no arguments")));
    }
    let unit_dir = config::unit_dir_from_env(env).map_err(fail_err)?;
    // SPIRA_ALERT_GLOB is a registered key (spira/conf.d); its own registered default is
    // empty, meaning "none" — intake::NO_GLOB already covers that case below.
    let pattern = spira_config::process::cfg("SPIRA_ALERT_GLOB").map_err(fail_err).map(|s| if s.is_empty() { None } else { Some(s) })?;
    // SPIRA_SYSTEMCTL_RELOAD is not a registered key (no spira/conf.d/ entry) — ambient env.
    let reload = env.get("SPIRA_SYSTEMCTL_RELOAD").map(|s| s != "0").unwrap_or(true);
    let sc = RealSystemctl::from_env();
    match sub.as_str() {
        "install" => {
            let release_root = env
                .get("SPIRA_RELEASE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| fail_err("SPIRA_RELEASE is not set — intake needs it to find the release's own incident.sh".into()))?;
            let o = intake::Opts { unit_dir, pattern, incident: release_root.join("spira/incident.sh"), reload };
            match intake::install(&RealTemplates, &sc, &o).map_err(fail_err)? {
                None => println!("install-intake: {}", intake::NO_GLOB),
                Some(r) => {
                    let unverified = if r.unverified.is_empty() { String::new() } else { format!(", {} unverified: {}", r.unverified.len(), r.unverified.join(" ")) };
                    println!("release: intake install: {} template(s), {} wired{unverified}", r.total, r.wired.len());
                    if !r.unverified.is_empty() {
                        return Err((1, String::new()));
                    }
                }
            }
        }
        "status" => {
            let o = intake::Opts { unit_dir, pattern, incident: PathBuf::new(), reload };
            match intake::status(&RealTemplates, &o).map_err(fail_err)? {
                None => println!("install-intake: {}", intake::NO_GLOB),
                Some((lines, wired, total)) => {
                    for l in &lines {
                        println!("{l}");
                    }
                    println!("{wired} of {total} alert template(s) wired to incident.sh");
                    if total == 0 || wired != total {
                        return Err((1, String::new()));
                    }
                }
            }
        }
        "uninstall" => {
            let o = intake::Opts { unit_dir, pattern, incident: PathBuf::new(), reload };
            match intake::uninstall(&RealTemplates, &sc, &o).map_err(fail_err)? {
                None => println!("install-intake: {}", intake::NO_GLOB),
                Some(removed) => println!("release: intake: removed {} drop-in(s)", removed.len()),
            }
        }
        other => return Err(usage_err(format!("unknown intake subcommand {other}"))),
    }
    Ok(())
}

/// The installing release's own tree, unpacked to a scratch directory for as long as this
/// process runs: removed on drop.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `spira/` of the release in `tarball`, read out of it: its key registry and its personas
/// (`chamber/`) are what a fresh box's config is written against, and what this process reads that config with.
fn tarball_home(tarball: &Path) -> Result<(Scratch, PathBuf), String> {
    let scratch = Scratch(std::env::temp_dir().join(format!("release-install-home-{}", std::process::id())));
    std::fs::create_dir_all(&scratch.0).map_err(|e| format!("{}: {e}", scratch.0.display()))?;
    // batch-job: tar reads one directory out of the tarball being installed
    let st = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(&scratch.0)
        .args(["--wildcards", "*/spira/conf.d/*", "*/spira/chamber/*"])
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !st.success() {
        return Err(format!("{} carries no spira/conf.d or spira/chamber ({st})", tarball.display()));
    }
    let top = std::fs::read_dir(&scratch.0).map_err(|e| e.to_string())?.flatten().next().ok_or("empty extraction")?.path();
    Ok((scratch, top.join("spira")))
}

/// Install-tarball is the first step of a fresh install, so it runs where nothing is set up:
/// no harness home of its own (a `release` binary taken out of the tarball) and no config.
/// The home is then the installing release's own tree; the config is the file `SPIRA_TOML`
/// names, else the canonical location — produced from `answers` (or a prompt on a terminal)
/// when absent, every other key at that release's registered default. When `SPIRA_TOML` was
/// unset it names the file for the rest of this process, and the operator is told to export
/// it for the steps that follow.
fn ensure_install_env(env: &config::Env, tarball: &Path, answers: Option<&Path>) -> Result<Option<Scratch>, String> {
    use spira_config::init;
    let own = spira_config::resolve::locate_home_for_process().ok().map(|h| spira_config::resolve::default_conf_d(&h)).filter(|d| d.is_dir());
    let mut scratch = None;
    let conf_d = match own {
        Some(d) => d,
        None => {
            let (s, home) = tarball_home(tarball)?;
            std::env::set_var("SPIRA_HOME", &home);
            scratch = Some(s);
            spira_config::resolve::default_conf_d(&home)
        }
    };
    let out = init::default_out(env)?;
    if let init::Outcome::Written(p) = init::ensure_from_cli(Some(out.clone()), answers, &Default::default(), &conf_d)? {
        println!("release: wrote {} from the operator's answers", p.display());
    }
    if env.get("SPIRA_TOML").is_none_or(|t| t.is_empty()) {
        std::env::set_var("SPIRA_TOML", &out);
        println!("release: SPIRA_TOML names the one config — export SPIRA_TOML={} for the steps that follow", out.display());
    }
    Ok(scratch)
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
            // SPIRA_VERDICT_WINDOW is a registered key (spira/conf.d); its own registered
            // default (400) replaces the literal that used to live here.
            let verdict_window = spira_config::process::cfg_parse::<usize>("SPIRA_VERDICT_WINDOW").map_err(fail)?;
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
        // session-hook/intake are handled here, before Config::resolve, for the same reason
        // stage/canary are: `uninstall`/`prune` (session-hook) touch only the client's own
        // settings file and need no release at all, and `intake` needs only the unit
        // directory — resolving the full Config would refuse every one of them on a box
        // whose releases directory cannot be found, which is exactly the state an uninstall
        // may be reached from.
        "config-stage" => {
            want(1)?;
            let spec = env.get("SPIRA_TOML").filter(|s| !s.is_empty()).ok_or_else(|| fail("config-stage needs SPIRA_TOML naming the layers to apply the delta to".into()))?;
            let staged = release::config_delta::staged_for_tree(spec, Path::new(&rest[0])).map_err(fail)?;
            println!("{}", staged.map_or_else(|| spec.clone(), |(_, s)| s));
            return Ok(());
        }
        "session-hook" => return session_hook_cmd(&env, &a, rest),
        "intake" => return intake_cmd(&env, rest),
        _ => {}
    }

    // A fresh box has no config yet: install-tarball is the first step of an install, so it
    // PRODUCES the config from the operator's answers (per Ryan 2026-10-07) — before anything
    // reads config. An existing file is validated and used, never overwritten.
    let mut _home = None;
    if cmd == "install-tarball" && !a.dry_run {
        let tb = rest.first().ok_or_else(|| usage("install-tarball takes 1 argument(s)".into()))?;
        _home = ensure_install_env(&env, Path::new(tb), a.answers.as_deref()).map_err(fail)?;
    }
    let cfg = resolve_config(&a, &env, &cmd, rest).map_err(|e| (1, e))?;
    let repo = || release::repo::resolve(a.repo.as_deref().map(Path::new), &env).or_else(|| a.repo.clone().map(PathBuf::from)).or_else(|| env.get("SPIRA_REPO").filter(|s| !s.is_empty()).map(PathBuf::from));
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
            let ctx = Ctx { cfg: &cfg, sc: &sc, git: &RealGit, repo: repo(), landed_ref: a.landed_ref.clone(), settle: a.settle, drain: a.drain };
            if cmd == "activate" {
                want(1)?;
                let (s, p) = release::prune::activate_and_prune(&ctx, &rest[0], a.hotfix.as_deref()).map_err(fail)?;
                println!("release: {} active; {} unit file(s) rewritten, restarted [{}], deferred to next start [{}]; {} release(s) pruned", rest[0], s.rewritten.len(), s.restarted.join(" "), s.deferred.join(" "), p.removed.len());
                if !p.failed.is_empty() {
                    eprintln!("release: WARN: prune could not remove: {}", p.failed.join("; "));
                }
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
            let o = InstallOpts { dry_run: a.dry_run, settle: a.settle, skip_restart: a.skip_restart };
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
            // An empty message means the detail was already printed (e.g. `session-hook
            // status`'s own lines) — nothing to add on stderr, just the exit code.
            if !msg.is_empty() {
                eprintln!("release: {msg}");
            }
            ExitCode::from(code)
        }
    }
}
