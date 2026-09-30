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
    Ok(Cli { home, fayth, mode, prompt_from_stdin: stdin })
}

fn main() {
    let t0 = util::now_epoch();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cli = match parse(&args) {
        Ok(c) => c,
        Err(u) => fatal(&u),
    };
    if cli.prompt_from_stdin {
        let mut p = String::new();
        let _ = std::io::stdin().read_to_string(&mut p);
        cli.mode = Mode::Sweep { prompt: Some(p.trim_end_matches('\n').to_string()) };
    }
    let original: BTreeMap<String, String> = std::env::vars().collect();
    let exe = std::env::current_exe().ok();
    let Some(home) = conf::resolve_home(cli.home.as_deref(), &original, exe.as_deref()) else {
        fatal("cannot find the harness's spira/ directory (pass --home, or set SPIRA_HOME)")
    };
    let fayth_file = home.join("chamber").join(format!("{}.fayth", cli.fayth));
    if !fayth_file.is_file() {
        fatal(&format!("no such fayth: {}", fayth_file.display()));
    }
    let own_unit = original.get("AEON_OWN_UNIT").cloned().unwrap_or_else(own_unit_from_cgroup);

    // conf.sh's resolution, once, through the seam.
    let boot_env = Env::new(original.clone(), BTreeMap::new());
    let boot = BashSeam { lib: home.join("lib.sh"), fayth_file: fayth_file.clone(), fayth: cli.fayth.clone(), env: &boot_env };
    let vars: Vec<String> = seam::SNAPSHOT_VARS.iter().map(|s| s.to_string()).collect();
    let raw = aeon::ports::Seam::call(&boot, "_aeon_snapshot", &vars);
    let snap = match seam::parse_snapshot(&raw.stdout) {
        Ok(s) => s,
        Err(e) => fatal(&format!("{}: {e}: {}", cli.fayth, raw.stderr.lines().next().unwrap_or(""))),
    };
    let env = Env::new(original.clone(), snap.env.clone());
    let conf = Conf::new(&snap, &home);
    let fayth = Fayth::from_vars(&cli.fayth, &snap.vars);
    let toml = conf.s("SPIRA_TOML_FILE");
    let enforce = conf::lifecycle_enforce(&original, (!toml.is_empty()).then(|| Path::new(&toml)));
    let claim_bin = "spira-claim".to_string();

    let seam = BashSeam { lib: home.join("lib.sh"), fayth_file: fayth_file.clone(), fayth: cli.fayth.clone(), env: &env };
    let bd = BdCli {
        bd: conf.or("SPIRA_BD", "bd"),
        db: conf.db(),
        timeout_s: conf.n("BD_TIMEOUT", 180).max(1) as u64,
        conn_retries: conf.n("SPIRA_BDQ_CONN_RETRIES", 2).max(1) as u32,
        fixture: Some(conf.s("SPIRA_BDJSON_FIXTURE")).filter(|s| !s.is_empty()),
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
        d: Deps { bd: &bd, seam: &seam, git: &git, exec: &exec, launcher: &RealLauncher, sink: &sink, env: &env, clock: &clock },
        ledger: Ledger { path: conf.ledger(), dry },
        conf,
        fayth,
        snap,
        mode: cli.mode,
        pid: std::process::id(),
        own_unit,
        t0,
        enforce,
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
}
