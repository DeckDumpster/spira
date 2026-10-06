//! `unit-ensure` — the non-disruptive unit installer (`systemd/unit-ensure.sh`, in Rust).
//! Safe while aeons are active: installs new/changed unit files and daemon-reloads, but
//! never restarts a running service. `landing-pass` runs this after every pass.
//!
//! usage: unit-ensure [--diff]

use install::bootstrap;
use install::ensure;
use install::install_units::Ctx;
use install::systemctl::RealSystemctl;
use std::process::ExitCode;

fn main() -> ExitCode {
    let diff = std::env::args().skip(1).any(|a| a == "--diff");
    let instance = bootstrap::nonempty_env("SPIRA_INSTANCE").unwrap_or_else(|| "prod".into());

    if diff {
        // unit-ensure.sh --diff execs install.sh --diff — the same diff, from units-install.
        let status = std::process::Command::new("units-install").arg(&instance).arg("--diff").status();
        return match status {
            Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
            Err(e) => {
                eprintln!("unit-ensure: cannot run units-install: {e}");
                ExitCode::from(1)
            }
        };
    }

    if let Some(msg) = bootstrap::refuse_if_stale_release("unit-ensure") {
        eprintln!("{msg}");
        return ExitCode::from(1);
    }

    let templates_dir = bootstrap::templates_dir();
    let manifest = match bootstrap::manifest_from_env(&instance) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("unit-ensure: {e}");
            return ExitCode::from(1);
        }
    };
    let host = match bootstrap::host_from_env(&instance) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("unit-ensure: {e}");
            return ExitCode::from(1);
        }
    };
    let dir = bootstrap::unit_dir();
    let _ = std::fs::create_dir_all(&dir);
    let systemctl = RealSystemctl::from_env();
    let world_halted = bootstrap::world_halted();
    let no_suspend = |_: &str| false; // unit-ensure.sh never read ctrl.sh's suspended set either.
    let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &no_suspend, world_halted, skip_migrate_watchers: false };

    let r = ensure::run(&ctx, bootstrap::declared("SPIRA_BROKER_ENABLE").is_some_and(|v| v == "1"));
    for e in &r.errors {
        eprintln!("unit-ensure: {e}");
    }
    for u in &r.installed {
        println!("unit-ensure: installed  {u}");
    }
    for u in &r.updated {
        println!("unit-ensure: updated    {u}");
    }
    if !r.installed.is_empty() || !r.updated.is_empty() {
        println!("unit-ensure: daemon-reload after {} change(s), {} unchanged", r.installed.len() + r.updated.len(), r.unchanged);
    } else {
        println!("unit-ensure: no changes — {} unit(s) current", r.unchanged);
    }
    for m in &r.missing_target {
        eprintln!("unit-ensure: MISSING-TARGET  {m}");
    }
    for u in &r.enabled_started {
        println!("unit-ensure: enabled+started  {u}");
    }
    for u in &r.broker_enabled {
        println!("unit-ensure: enabled+started  {u}");
    }
    for u in &r.broker_disabled {
        println!("unit-ensure: DISABLED {u} (no producer; SPIRA_BROKER_ENABLE=1 to opt in)");
    }

    if r.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
