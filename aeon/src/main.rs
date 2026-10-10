//! `aeon [--home DIR] <fayth> [--dry-run | --sweep [--prompt <text>|-]]` — DESIGN.md §2.1.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use aeon::bd::BdCli;
use aeon::conf::{self, Conf, Fayth};
use aeon::ledger::Ledger;
use aeon::ports::{Env, RealExec, RealGit};
use aeon::run::{Deps, Mode, Run, State};
use aeon::seam::{self, BashSeam};
use aeon::session::{self, RealLauncher, Stop};
use aeon::util::{self, Sink, StdSink};

fn fatal(msg: &str) -> ! {
    StdSink.err(&util::log_line(util::now_epoch(), &format!("FATAL {msg}")));
    std::process::exit(1)
}

/// Parsed argv, or the usage error.
pub struct Cli {
    pub home: Option<String>,
    pub fayth: String,
    pub mode: Mode,
    pub prompt_from_stdin: bool,
    /// `--escape`: direct-summon this fayth, bypassing pool and lane checks (replaces
    /// `spira/escape.sh`, sp-zpaq0). Every other field is ignored in this mode except
    /// `home` and `fayth`.
    pub escape: bool,
    /// `--dry-run` after `--escape <fayth>`: passed through to the summoned aeon, not a
    /// dry run of the escape decision itself.
    pub escape_dry_run: bool,
}

pub fn parse(args: &[String]) -> Result<Cli, String> {
    let usage = "usage: aeon.sh <fayth> [--dry-run | --sweep [--prompt <text>|-]]".to_string();
    let mut a: Vec<String> = args.to_vec();
    let mut home = None;
    if a.first().map(|s| s.as_str()) == Some("--home") {
        if a.len() < 2 {
            return Err(usage);
        }
        home = Some(a[1].clone());
        a.drain(..2);
    }
    if a.first().map(|s| s.as_str()) == Some("--escape") {
        let escape_usage = "usage: aeon --escape <fayth> [--dry-run]".to_string();
        let fayth = a.get(1).cloned().filter(|f| !f.is_empty()).ok_or(escape_usage)?;
        let escape_dry_run = a.get(2).map(|s| s.as_str()) == Some("--dry-run");
        return Ok(Cli { home, fayth, mode: Mode::Claim, prompt_from_stdin: false, escape: true, escape_dry_run });
    }
    let fayth = a.first().cloned().filter(|f| !f.is_empty()).ok_or_else(|| usage.clone())?;
    let (mut mode, mut stdin) = (Mode::Claim, false);
    match a.get(1).map(|s| s.as_str()) {
        Some("--dry-run") => mode = Mode::DryRun,
        Some("--sweep") => {
            let prompt = match a.get(2).map(|s| s.as_str()) {
                Some("--prompt") => match a.get(3).map(|s| s.as_str()) {
                    Some("-") => {
                        stdin = true;
                        None
                    }
                    Some(t) => Some(t.to_string()),
                    None => Some(String::new()),
                },
                Some("-") => {
                    stdin = true;
                    None
                }
                _ => None,
            };
            mode = Mode::Sweep { prompt };
        }
        _ => {}
    }
    Ok(Cli { home, fayth, mode, prompt_from_stdin: stdin, escape: false, escape_dry_run: false })
}

/// `aeon capacity <verb> ...` (wave 4.26): no fayth, so this resolves config without the
/// bash seam round trip at all — `merge_resolved_config` already runs entirely in-process
/// (`spira_config::resolve::resolve_for_process`); the bash seam exists only to source a
/// fayth file and lib.sh's own derived values, neither of which this subcommand needs.
fn run_stop(args: &[String]) -> i32 {
    let original: BTreeMap<String, String> = std::env::vars().collect();
    let exe = std::env::current_exe().ok();
    let Some(home) = conf::resolve_home(None, &original, exe.as_deref()) else {
        fatal("cannot find the harness's spira/ directory (set SPIRA_HOME)")
    };
    let mut snap = seam::Snapshot::default();
    if let Err(e) = conf::merge_resolved_config(&mut snap, &home, &original) {
        fatal(&format!("config resolution: {e}"));
    }
    let conf = Conf::new(&snap, &home);
    aeon::stop::run(&conf.run, args, util::now_epoch())
}

fn run_capacity(args: &[String]) -> i32 {
    let original: BTreeMap<String, String> = std::env::vars().collect();
    // The lifecycle machine is the only mode (sp-v62vn): a retired switch saying off is
    // refused here, by name, before anything runs; one saying on is a deprecation warning.
    match spira_config::check_lifecycle_switch_env(original.get(spira_config::LIFECYCLE_ENFORCE_ENV).map(String::as_str)) {
        Ok(Some(w)) => eprintln!("aeon: {w}"),
        Ok(None) => {}
        Err(e) => fatal(&format!("aeon: {e}")),
    }
    let exe = std::env::current_exe().ok();
    let Some(home) = conf::resolve_home(None, &original, exe.as_deref()) else {
        fatal("cannot find the harness's spira/ directory (set SPIRA_HOME)")
    };
    let mut snap = seam::Snapshot::default();
    if let Err(e) = conf::merge_resolved_config(&mut snap, &home, &original) {
        fatal(&format!("config resolution: {e}"));
    }
    conf::merge_capacity_env(&mut snap, &original);
    let conf = Conf::new(&snap, &home);
    let env = Env::new(original.clone(), snap.env.clone());
    let exec = RealExec { env: &env, timeout: None };
    aeon::capacity_cli::run(&conf, &exec, util::now_epoch(), args)
}

/// `aeon fast-tier <repo> <work> <branch> <base>`: the handoff's fast tier (`fast_tier::red`)
/// against a checkout, with an absent tool or a tree without the fence refused rather than
/// skipped. Exit 0 green, 1 red (the text on stdout), 2 usage, 3 a tool failure that judges nothing.
fn run_fast_tier(args: &[String]) -> i32 {
    let [repo, work, branch, base] = args else {
        eprintln!("usage: aeon fast-tier <repo> <work> <branch> <base>");
        return 2;
    };
    let original: BTreeMap<String, String> = std::env::vars().collect();
    let env = Env::new(original.clone(), original);
    let (git, exec) = (RealGit { env: &env }, RealExec { env: &env, timeout: None });
    match aeon::fast_tier::red(&git, &exec, Path::new(repo), Path::new(work), branch, base, true) {
        Some(red) => {
            println!("{}", red.text);
            if red.harness { 3 } else { 1 }
        }
        None => {
            println!("fast tier green: {branch} against {base}");
            0
        }
    }
}

fn main() {
    let t0 = util::now_epoch();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `aeon aeon-named <pidfile>` (wave 4.23, sp-0ffox): lib.sh's own one-line shim target
    // for `aeon_named` — the only caller left after aeon_name_take's in-process switch is
    // cockpit-collect, across the crate boundary, so this stays a real subcommand rather
    // than an in-process call. Stateless: no --home/conf resolution needed.
    if args.first().map(String::as_str) == Some("aeon-named") {
        let Some(pf) = args.get(1) else {
            fatal("usage: aeon aeon-named <pidfile>");
        };
        print!("{}", aeon::naming::aeon_named(Path::new(pf)));
        std::process::exit(0);
    }
    if args.first().map(String::as_str) == Some("fast-tier") {
        std::process::exit(run_fast_tier(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("stop") {
        std::process::exit(run_stop(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("capacity") {
        std::process::exit(run_capacity(&args[1..]));
    }
    let mut cli = match parse(&args) {
        Ok(c) => c,
        Err(u) => fatal(&u),
    };
    if cli.prompt_from_stdin {
        let mut p = String::new();
        let _ = std::io::stdin().read_to_string(&mut p);
        cli.mode = Mode::Sweep { prompt: Some(p.trim_end_matches('\n').to_string()) };
    }
    // THE LAUNCHER SETS PATH (sp-31gtu): the aeon's launcher (the summon's systemd-run, the
    // ops unit) hands it a PATH set outright from SPIRA_RELEASE, and the aeon carries it to
    // the session and its subagents unchanged — it never rebuilds or appends to it.
    let original: BTreeMap<String, String> = std::env::vars().collect();
    let exe = std::env::current_exe().ok();
    let Some(home) = conf::resolve_home(cli.home.as_deref(), &original, exe.as_deref()) else {
        fatal("cannot find the harness's spira/ directory (pass --home, or set SPIRA_HOME)")
    };
    let fayth_file = home.join("chamber").join(format!("{}.fayth", cli.fayth));
    if !fayth_file.is_file() {
        fatal(&format!("no such fayth: {}", fayth_file.display()));
    }
    if let Err(e) = aeon::fayth_keys::check(&fayth_file, &home) {
        fatal(&e);
    }
    let own_unit = original.get("AEON_OWN_UNIT").cloned().unwrap_or_else(own_unit_from_cgroup);

    // conf.sh's resolution, once, through the seam.
    let boot_env = Env::new(original.clone(), BTreeMap::new());
    let boot = BashSeam { lib: home.join("lib.sh"), fayth_file: fayth_file.clone(), fayth: cli.fayth.clone(), env: &boot_env };
    let vars: Vec<String> = seam::SNAPSHOT_VARS.iter().map(|s| s.to_string()).collect();
    let raw = aeon::ports::Seam::call(&boot, "_aeon_snapshot", &vars);
    let mut snap = match seam::parse_snapshot(&raw.stdout) {
        Ok(s) => s,
        Err(e) => fatal(&format!("{}: {e}: {}", cli.fayth, raw.stderr.lines().next().unwrap_or(""))),
    };
    if let Err(e) = conf::merge_resolved_config(&mut snap, &home, &original) {
        fatal(&format!("{}: config resolution: {e}", cli.fayth));
    }
    conf::merge_capacity_env(&mut snap, &original);
    let env = Env::new(original.clone(), snap.env.clone());
    let conf = Conf::new(&snap, &home);
    let fayth = Fayth::from_vars(&cli.fayth, &snap.vars);
    if snap.vars.get("FAYTH_SOP_REQUIRED").is_some_and(|v| !v.is_empty()) {
        eprintln!("{}: FAYTH_SOP_REQUIRED is retired (sp-loycl) and ignored — remove it from the fayth", cli.fayth);
    }
    let claim_bin = "spira-claim".to_string();

    let seam = BashSeam { lib: home.join("lib.sh"), fayth_file: fayth_file.clone(), fayth: cli.fayth.clone(), env: &env };

    if cli.escape {
        // world_gate, fayth_ready and summon_argv carry real side effects (an expired
        // drain is lifted and logged) that must stay lib.sh's, reached through the same
        // seam summon_fayth uses — not reimplemented in Rust where they could drift
        // (escape.rs's own doc, family G). The capacity check is in-process
        // (`capacity::check_and_probe`, wave 4.26) so the probe fires from exactly one
        // place no matter which of this binary's own entry points asks.
        let exec = RealExec { env: &env, timeout: None };
        let sink = StdSink;
        let summon_bin = original.get("SPIRA_SUMMON").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemd-run".to_string());
        let rc = aeon::escape::run(&seam, &exec, &env, &sink, &conf, &summon_bin, &home, &cli.fayth, cli.escape_dry_run, util::now_epoch());
        std::process::exit(rc);
    }
    // SPIRA_BD is registered but carries no conf.d default ("resolves empty unless set via
    // environment or the config file") — the real config file always sets it explicitly (per Ryan
    // 2026-10-05: one source of config), so an empty resolution here refuses by name rather
    // than guessing "bd". BD_TIMEOUT/SPIRA_BDQ_CONN_RETRIES/SPIRA_BDJSON_FIXTURE are not
    // registered config keys (spira/conf.d has no entry for any of them).
    let bd_bin = conf.s("SPIRA_BD");
    if bd_bin.is_empty() {
        fatal("SPIRA_BD resolved empty — refusing rather than guessing a bd binary");
    }
    let bd = BdCli {
        bd: bd_bin,
        db: conf.db(),
        timeout_s: conf.n("BD_TIMEOUT", 180).max(1) as u64,
        conn_retries: conf.n("SPIRA_BDQ_CONN_RETRIES", 2).max(1) as u32,
        // Not a registered config key — `Conf::or`, not the strict `Conf::s`.
        fixture: Some(conf.or("SPIRA_BDJSON_FIXTURE", "")).filter(|s| !s.is_empty()),
        home: home.clone(),
        env: &env,
    };
    let git = RealGit { env: &env };
    let exec = RealExec { env: &env, timeout: None };
    let sink = StdSink;
    let stop = Arc::new(Stop::default());
    session::watch_signals(Arc::clone(&stop));
    let clock = util::now_epoch;
    let dry = cli.mode == Mode::DryRun;
    let mut run = Run {
        d: Deps { bd: &bd, seam: &seam, git: &git, exec: &exec, launcher: &RealLauncher, sink: &sink, env: &env, clock: &clock, sleep: &|d| std::thread::sleep(d) },
        ledger: Ledger { path: conf.ledger(), dry },
        conf,
        fayth,
        snap,
        mode: cli.mode,
        pid: std::process::id(),
        own_unit,
        t0,
        claim_bin,
        stop,
        hb_shutdown: Arc::new(AtomicBool::new(false)),
        hb_done: Arc::new(AtomicBool::new(false)),
        fayth_file,
        s: State::default(),
    };
    let code = run.main();
    std::process::exit(code);
}

/// `aeon_own_unit`: the spira-aeon-*.service this process runs under, from its cgroup.
fn own_unit_from_cgroup() -> String {
    let t = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let re = regex::Regex::new(r"spira-aeon-[^/\s]+\.service").unwrap();
    re.find_iter(&t).last().map(|m| m.as_str().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn argv_grammar() {
        assert!(parse(&[]).is_err());
        let c = parse(&a(&["builder"])).unwrap();
        assert_eq!((c.fayth.as_str(), c.mode), ("builder", Mode::Claim));
        assert_eq!(parse(&a(&["builder", "--dry-run"])).unwrap().mode, Mode::DryRun);
        let c = parse(&a(&["--home", "/h/spira", "ops", "--sweep", "--prompt", "-"])).unwrap();
        assert_eq!(c.home.as_deref(), Some("/h/spira"));
        assert!(c.prompt_from_stdin);
        assert_eq!(parse(&a(&["ops", "--sweep", "--prompt", "look"])).unwrap().mode, Mode::Sweep { prompt: Some("look".into()) });
        assert!(parse(&a(&["ops", "--sweep", "-"])).unwrap().prompt_from_stdin);
        assert_eq!(parse(&a(&["ops", "--sweep"])).unwrap().mode, Mode::Sweep { prompt: None });
    }

    #[test]
    fn escape_grammar() {
        let c = parse(&a(&["--escape", "stretchy"])).unwrap();
        assert!(c.escape);
        assert_eq!(c.fayth, "stretchy");
        assert!(!c.escape_dry_run);
        let c = parse(&a(&["--escape", "stretchy", "--dry-run"])).unwrap();
        assert!(c.escape_dry_run);
        let c = parse(&a(&["--home", "/h", "--escape", "stretchy"])).unwrap();
        assert_eq!(c.home.as_deref(), Some("/h"));
        assert!(c.escape);
        assert!(parse(&a(&["--escape"])).is_err());
    }
}
