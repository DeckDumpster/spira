//! `gate [--home <spira-dir>] [--release-bins] <branch> [repo-name]` — see DESIGN.md.
//! `gate [--home <spira-dir>] --definition [repo-name]` — the landing ref's gate command.

use gate::engine::{Args, Trial};
use gate::real::{install_signal_handlers, Real};
use std::path::PathBuf;

fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

const IN_UNIT_ENV: &str = "SPIRA_GATE_IN_UNIT";

fn confine(self_exe: &std::path::Path, args: &[String]) -> Option<Result<std::convert::Infallible, String>> {
    use std::os::unix::process::CommandExt;
    let quota = std::env::var(gate::cgroup::QUOTA_ENV).ok().filter(|v| !v.is_empty())?;
    if std::env::var_os(IN_UNIT_ENV).is_some() {
        return None;
    }
    let argv = match gate::cgroup::scope_argv(&quota, self_exe, args) {
        Ok(a) => a,
        Err(e) => return Some(Err(e)),
    };
    // batch-job: child is spawned or exec-replaced, not awaited under a deadline
    let err = std::process::Command::new(&argv[0]).args(&argv[1..]).env(IN_UNIT_ENV, "1").exec();
    Some(Err(format!("cannot exec {}: {err}", argv[0])))
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // `gate wait <out>` (sp-tcarr): block on a backgrounded run's own pid, not its
    // `gate: VERDICT=` line — a run SIGKILLed mid-flight never writes one. A branch
    // literally named `wait` is `gate -- wait` (same tradeoff `testenv container` takes).
    if argv.first().map(String::as_str) == Some("wait") {
        let rc = gate::wait::main(&argv[1..]);
        std::process::exit(rc);
    }
    if matches!(argv.first().map(String::as_str), Some("status" | "cancel")) {
        let run = match spira_config::process::cfg("SPIRA_RUN") {
            Ok(r) => PathBuf::from(r),
            Err(e) => {
                eprintln!("gate {}: {e}", argv[0]);
                std::process::exit(1);
            }
        };
        let rc = if argv[0] == "status" { gate::machine::status_main(&run, &argv[1..]) } else { gate::machine::cancel_main(&run, &argv[1..]) };
        std::process::exit(rc);
    }
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate: --home needs a directory");
            std::process::exit(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    // `--release-bins` (sp-z61hj): on a PASS, build the judged tree's release binaries in the
    // gate tree for a hand landing (engine.rs `release_bins`).
    let mut release_bins = false;
    if argv.first().map(String::as_str) == Some("--release-bins") {
        release_bins = true;
        argv.remove(0);
    }
    // `warm-tools [--rev <rev>] [--repo <name> | <name>]`: build and publish the landing ref's
    // (or <rev>'s) gate tools into the shared store, off any gate's clock (toolkey.rs).
    if argv.first().map(String::as_str) == Some("warm-tools") {
        let (mut rev, mut repo) = (None, None);
        let mut it = argv[1..].iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--rev" => rev = it.next().cloned(),
                "--repo" => repo = it.next().cloned(),
                r if !r.starts_with('-') && repo.is_none() => repo = Some(r.to_string()),
                _ => {
                    eprintln!("usage: gate [--home <spira-dir>] warm-tools [--rev <rev>] [--repo <name>]");
                    std::process::exit(2);
                }
            }
        }
        let world = Real::new(home.clone().unwrap_or_else(default_home));
        let branch = rev.clone().unwrap_or_default();
        let code = Trial::new(&world, Args { home: home.unwrap_or_else(default_home), branch, repo, release_bins: false }).warm(rev.as_deref());
        std::process::exit(code);
    }
    if argv.first().map(String::as_str) == Some("--definition") {
        let repo = argv.get(1).filter(|r| !r.is_empty()).cloned();
        let world = Real::new(home.clone().unwrap_or_else(default_home));
        match gate::engine::definition(&world, repo.as_deref()) {
            Ok(cmd) => {
                println!("{cmd}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("gate: --definition: {e}");
                std::process::exit(1);
            }
        }
    }
    let Some(branch) = argv.first().filter(|b| !b.is_empty()).cloned() else {
        eprintln!("gate.sh: 1: usage: gate.sh [--release-bins] <branch> [repo-name]");
        std::process::exit(1);
    };
    let repo = argv.get(1).filter(|r| !r.is_empty()).cloned();
    let home = home.unwrap_or_else(default_home);
    if let Some(Err(why)) = std::env::current_exe()
        .map_err(|e| e.to_string())
        .map(|exe| confine(&exe, &std::env::args().skip(1).collect::<Vec<_>>()))
        .unwrap_or_else(|e| Some(Err(e)))
    {
        let rp = repo.clone().unwrap_or_else(|| "?".to_string());
        eprintln!("gate: VERDICT=NO_VERDICT reason=cgroup-unavailable branch={branch} repo={rp} suite=-");
        eprintln!("gate: {} is set but the gate cannot run in its own unit: {why}", gate::cgroup::QUOTA_ENV);
        std::process::exit(1);
    }
    install_signal_handlers();
    // A panic unwinding out of `Trial::run` (a bug, not a signal) would otherwise exit via
    // Rust's default panic runtime with no `gate: VERDICT=` line at all — the same hang for
    // a waiter as a SIGKILL, just self-inflicted (sp-tcarr).
    {
        let br = branch.clone();
        let rp = repo.clone().unwrap_or_else(|| "?".to_string());
        std::panic::set_hook(Box::new(move |info| {
            eprintln!(
                "gate: VERDICT=NO_VERDICT reason=exit-101 branch={br} repo={rp} suite=-"
            );
            eprintln!("gate: panic: {info}");
        }));
    }
    let world = Real::new(home.clone());
    let code = Trial::new(&world, Args { home, branch, repo, release_bins }).run();
    std::process::exit(code);
}
