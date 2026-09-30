//! The root installer (`install.sh`, in Rust): nine phases — preflight, conflicts, config,
//! build, database, units, hooks, cockpit, verify. Every phase that shells to a tool not yet
//! ported (`doctor.sh`, `configure.sh`, `build.sh`, `seed.sh`, `mail.sh`,
//! `install-session-hook.sh`, `install-intake.sh`, `exclude.sh`, `ready.sh`, `loginctl`) does
//! so through [`Deps`], by bare name on the launcher `PATH` (sp-gypjk) — none of those tools
//! move in this bead; wave 6a is `install.sh`, `systemd/install.sh`, `unit-ensure.sh`,
//! `units.sh` and `render.py` only.

use crate::guards;
use crate::install_units::{self, Ctx};
use crate::manifest::{self, Manifest};
use crate::systemctl::Systemctl;
use crate::values::HostValues;
use std::path::{Path, PathBuf};

/// Every external side effect this phase sequence needs, behind a trait so the sequencing
/// itself — which phase runs, in what order, and what a refusal or `--dry-run` skips — is
/// tested without doctor.sh, a real dolt server or systemd.
pub trait Deps {
    /// Run a bare-name tool; `Ok(stdout+stderr combined)` iff it exited 0.
    fn tool(&self, name: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String, String>;
    fn tcp_up(&self, port: u16) -> bool;
    fn now_secs(&self) -> u64;
    fn sleep_secs(&self, n: u64);
}

#[derive(Default)]
pub struct Opts {
    pub instance: Option<String>,
    pub dry_run: bool,
    pub ephemeral: bool,
    pub laptop: bool,
    pub skip_build: bool,
    pub no_session_hook: bool,
    pub system_user: bool,
    pub conflict_considered: bool,
}


/// Exit codes (install.sh's own contract, DESIGN.md "Contract"): 0 ready, 1 preflight
/// refused, 2 a phase failed, 3 installed but not ready, 5 conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
}

pub fn fail(code: i32) -> Outcome {
    Outcome { code }
}
pub const READY: Outcome = Outcome { code: 0 };

/// Everything phase 0.5's five conflict checks need, pre-resolved by the caller (real
/// `/proc`, real `flock`, a real TCP probe) into the plain values [`crate::guards`] decides
/// over.
#[derive(Debug, Default)]
pub struct ConflictInputs {
    pub our_unit_dir: String,
    pub installed_foreign_exec_dir: Option<String>,
    pub our_home: String,
    pub aeon_cmdlines: Vec<(String, String)>,
    pub gate_lock_path: String,
    pub gate_lock_held: bool,
    pub conf_instance: Option<String>,
    pub conf_file: String,
    pub our_sentinel_unit: String,
    pub other_sentinel_run_dirs: Vec<(String, String)>,
    pub run_dir: String,
    pub dolt_data: String,
    pub dolt_port: u16,
    pub dolt_listening: bool,
    pub dolt_servers: Vec<(String, String)>,
}

pub fn phase_0_5_conflicts(instance: &str, c: &ConflictInputs) -> Result<(), guards::Conflict> {
    guards::conflict_foreign(instance, c.installed_foreign_exec_dir.as_deref(), &c.our_home)?;
    guards::conflict_aeon(&c.our_home, c.aeon_cmdlines.iter().map(|(p, s)| (p.as_str(), s.as_str())))?;
    guards::conflict_lock(&c.gate_lock_path, c.gate_lock_held)?;
    guards::conflict_instance_arg(instance, c.conf_instance.as_deref(), &c.conf_file)?;
    guards::conflict_instance_run(&c.run_dir, &c.our_sentinel_unit, c.other_sentinel_run_dirs.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;
    guards::conflict_dolt(&c.dolt_data, c.dolt_port, c.dolt_listening, c.dolt_servers.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;
    Ok(())
}

/// Phase 3's dolt bootstrap-and-`bd init` sequence (server mode only): start a temporary
/// server if none is listening, wait for it to accept TCP then answer a real query (the port
/// accepts before dolt can serve, and a `bd init` issued in that gap fails with "invalid
/// connection"), then `bd init --server ...`, retrying once after clearing a partial
/// `.beads/` on that specific failure.
pub fn wait_for_dolt_query_ready(deps: &dyn Deps, port: u16, max_secs: u64) -> bool {
    let start = deps.now_secs();
    loop {
        if deps.tool("dolt", &["--host", "127.0.0.1", "--port", &port.to_string(), "--no-tls", "--user", "root", "--password", "", "sql", "-q", "select 1"], &[]).is_ok() {
            return true;
        }
        if deps.now_secs().saturating_sub(start) >= max_secs {
            return false;
        }
        deps.sleep_secs(1);
    }
}

pub fn wait_for_tcp(deps: &dyn Deps, port: u16, max_secs: u64) -> bool {
    let start = deps.now_secs();
    loop {
        if deps.tcp_up(port) {
            return true;
        }
        if deps.now_secs().saturating_sub(start) >= max_secs {
            return false;
        }
        deps.sleep_secs(1);
    }
}

/// Phase 4's per-instance unit install, wired up from already-resolved host values and a
/// pre-built manifest — the same [`install_units::run`] `deploy.sh`'s re-render step and
/// `units-install` (the standalone binary) call, invoked here in-process rather than as a
/// subprocess (root `install.sh` used to shell to `systemd/install.sh`; one binary now, so
/// phase 4 is a function call, not a second process).
pub fn phase_4_units(unit_dir: &Path, templates_dir: &Path, host: &HostValues, manifest: &Manifest, systemctl: &dyn Systemctl, suspended: &dyn Fn(&str) -> bool, world_halted: bool) -> install_units::Report {
    let ctx = Ctx { unit_dir, templates_dir, host, manifest, systemctl, suspended, world_halted, skip_migrate_watchers: false };
    install_units::run(&ctx)
}

/// Build phase 4's manifest from the box's own signals — the one place `install`'s phase 4
/// and the standalone `units-install`/`unit-ensure` binaries all call [`manifest::build`], so
/// they cannot resolve a different manifest from the same box.
pub fn build_manifest(instance: &str, dolt_data_set: bool, testdb_data_set: bool, broker_enable: bool, inotify_present: bool, watch_names: Result<Vec<String>, String>) -> Result<Manifest, String> {
    manifest::build(&manifest::Inputs { instance: instance.to_string(), dolt_data_set, testdb_data_set, broker_enable, inotify_present, watch_names })
}

/// Where the installed unit directory and the release's own PATH tail live — resolved once,
/// here, from `spira-config path-tail`'s own logic (in-process; DESIGN.md "Render" carries
/// the same refusal spira-config's binary makes, now without a subprocess).
pub fn path_tail() -> Result<String, String> {
    let tail = std::env::var("SPIRA_PATH").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
        spira_config::discover(None).and_then(|p| spira_config::load(&p).ok()).and_then(|d| d.spira).and_then(|s| s.path).unwrap_or_default()
    });
    let refusals = spira_config::tail_refusals(&tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(tail)
}

/// Default unit directory (`$XDG_CONFIG_HOME/systemd/user`, else `$HOME/.config/systemd/user`).
pub fn default_unit_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("systemd/user")).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config/systemd/user")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_run_in_order_and_the_first_hit_wins() {
        let mut c = ConflictInputs { our_home: "/home".into(), ..Default::default() };
        c.installed_foreign_exec_dir = Some("/other".into());
        c.aeon_cmdlines = vec![("1".into(), "bash\0/home/aeon.sh".into())];
        let err = phase_0_5_conflicts("prod", &c).unwrap_err();
        assert!(err.message.contains("foreign") || err.message.contains("exec from"), "{}", err.message);
    }

    #[test]
    fn a_clean_box_has_no_conflicts() {
        let c = ConflictInputs { our_home: "/home".into(), our_unit_dir: "/u".into(), ..Default::default() };
        assert!(phase_0_5_conflicts("prod", &c).is_ok());
    }

    struct FakeDeps {
        queries_ok_after: std::cell::Cell<u32>,
        clock: std::cell::Cell<u64>,
    }
    impl Deps for FakeDeps {
        fn tool(&self, name: &str, _args: &[&str], _env: &[(&str, &str)]) -> Result<String, String> {
            if name == "dolt" {
                let n = self.queries_ok_after.get();
                if n == 0 {
                    Ok(String::new())
                } else {
                    self.queries_ok_after.set(n - 1);
                    Err("invalid connection".into())
                }
            } else {
                Ok(String::new())
            }
        }
        fn tcp_up(&self, _port: u16) -> bool {
            true
        }
        fn now_secs(&self) -> u64 {
            self.clock.get()
        }
        fn sleep_secs(&self, n: u64) {
            self.clock.set(self.clock.get() + n);
        }
    }

    #[test]
    fn dolt_query_ready_retries_until_the_port_answers_real_queries() {
        let d = FakeDeps { queries_ok_after: std::cell::Cell::new(2), clock: std::cell::Cell::new(0) };
        assert!(wait_for_dolt_query_ready(&d, 3307, 30));
    }

    #[test]
    fn dolt_query_ready_gives_up_at_the_bound() {
        let d = FakeDeps { queries_ok_after: std::cell::Cell::new(9999), clock: std::cell::Cell::new(0) };
        assert!(!wait_for_dolt_query_ready(&d, 3307, 5));
    }
}
