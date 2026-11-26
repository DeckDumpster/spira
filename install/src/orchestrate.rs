//! The root installer (`install.sh`, in Rust): nine phases — preflight, conflicts, config,
//! build, database, units, hooks, cockpit, verify. Every phase that shells to a tool not yet
//! ported (`doctor.sh`, `configure.sh`, `build.sh`, `seed.sh`, `mail.sh`,
//! `release session-hook`, `release intake`, `exclude.sh`, `ready.sh`, `loginctl`) does
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
    pub dwarf_cmdlines: Vec<(String, String)>,
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
    guards::conflict_dwarf(&c.our_home, c.dwarf_cmdlines.iter().map(|(p, s)| (p.as_str(), s.as_str())))?;
    guards::conflict_lock(&c.gate_lock_path, c.gate_lock_held)?;
    guards::conflict_instance_arg(instance, c.conf_instance.as_deref(), &c.conf_file)?;
    guards::conflict_instance_run(&c.run_dir, &c.our_sentinel_unit, c.other_sentinel_run_dirs.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;
    guards::conflict_dolt(&c.dolt_data, c.dolt_port, c.dolt_listening, c.dolt_servers.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;
    Ok(())
}

/// Phase 3 waits for the server to answer a real query before it creates the store: the port
/// accepts before dolt can serve.
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

/// Until the beads store answers `<lc> admin-store ping`, or `max_secs` pass.
pub fn wait_for_store(deps: &dyn Deps, lc: &str, max_secs: u64) -> bool {
    let start = deps.now_secs();
    loop {
        if deps.tool(lc, &["admin-store", "ping"], &[]).is_ok() {
            return true;
        }
        if deps.now_secs().saturating_sub(start) >= max_secs {
            return false;
        }
        deps.sleep_secs(1);
    }
}

/// Runs `spira-lc admin-create-beads` through `run` (its exit code and output). A refusal is
/// final. Any other failure is retried up to `tries` runs, and `drop_partial` is called only when
/// the run said it had begun creating, so a database it found holding anything is never dropped.
pub fn create_beads(run: &mut dyn FnMut() -> (i32, String), drop_partial: &mut dyn FnMut(), pause: &mut dyn FnMut(u32), tries: u32) -> Result<(), String> {
    let mut ran = 0;
    loop {
        ran += 1;
        let (code, out) = run();
        if code == 0 {
            return Ok(());
        }
        if out.lines().any(|l| l.starts_with(spira_store::lifecycle_row::CREATING_BEADS_DATABASE)) {
            drop_partial();
        }
        if code == spira_store::lc_call::REFUSED || ran >= tries {
            return Err(out);
        }
        pause(ran);
    }
}

/// The cell of `admin-store sql --json`'s `SELECT COUNT(*) AS n`, or `?` for anything else.
pub fn bead_count(json: &str) -> String {
    let rows = serde_json::from_str::<serde_json::Value>(json).unwrap_or_default();
    rows[0]["n"].as_str().unwrap_or("?").to_string()
}

/// The port-close wait before phase 4 starts the unit: `true` once nothing listens, `false` when
/// the bound runs out with the temporary server still holding it.
pub fn wait_for_tcp_closed(deps: &dyn Deps, port: u16, max_secs: u64) -> bool {
    let start = deps.now_secs();
    loop {
        if !deps.tcp_up(port) {
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
pub fn build_manifest(instance: &str, dolt_data_set: bool, dolt_durable_set: bool, sccache_dav_addr_set: bool, repo_is_git_checkout: bool, watch_names: Result<Vec<String>, String>) -> Result<Manifest, String> {
    manifest::build(&manifest::Inputs { instance: instance.to_string(), dolt_data_set, dolt_durable_set, sccache_dav_addr_set, repo_is_git_checkout, lc_system_mode: spira_config::resolve::lc_system_mode(), watch_names })
}

fn configured_path() -> String {
    let doc = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok());
    doc.and_then(|d| d.spira).and_then(|s| spira_config::said(s.get_str("SPIRA_PATH"))).unwrap_or_default()
}

/// Where the installed unit directory and the release's own PATH tail live — resolved once,
/// here, from `spira-config path-tail`'s own logic (in-process; DESIGN.md "Render" carries
/// the same refusal spira-config's binary makes, now without a subprocess).
pub fn path_tail() -> Result<String, String> {
    let tail = std::env::var("SPIRA_PATH").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(configured_path);
    checked_tail(tail)
}

/// `tail` as a unit PATH may carry it, or every reason it may not.
pub fn checked_tail(tail: String) -> Result<String, String> {
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

    #[test_id(tcrftuu)]
    #[test]
    fn conflicts_run_in_order_and_the_first_hit_wins() {
        let mut c = ConflictInputs { our_home: "home".into(), ..Default::default() };
        c.installed_foreign_exec_dir = Some("other".into());
        c.dwarf_cmdlines = vec![("1".into(), "bash\0home/dwarf.sh".into())];
        let err = phase_0_5_conflicts("prod", &c).unwrap_err();
        assert!(err.message.contains("foreign") || err.message.contains("exec from"), "{}", err.message);
    }

    #[test_id(taqhhr6)]
    #[test]
    fn a_clean_box_has_no_conflicts() {
        let c = ConflictInputs { our_home: "home".into(), our_unit_dir: "u".into(), ..Default::default() };
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

    #[test_id(ttsb7g0)]
    #[test]
    fn dolt_query_ready_retries_until_the_port_answers_real_queries() {
        let d = FakeDeps { queries_ok_after: std::cell::Cell::new(2), clock: std::cell::Cell::new(0) };
        assert!(wait_for_dolt_query_ready(&d, 3307, 30));
    }

    #[test_id(to50c1x)]
    #[test]
    fn dolt_query_ready_gives_up_at_the_bound() {
        let d = FakeDeps { queries_ok_after: std::cell::Cell::new(9999), clock: std::cell::Cell::new(0) };
        assert!(!wait_for_dolt_query_ready(&d, 3307, 5));
    }

    struct Listener {
        up_from: u64,
        up_until: u64,
        clock: std::cell::Cell<u64>,
    }
    impl Deps for Listener {
        fn tool(&self, _: &str, _: &[&str], _: &[(&str, &str)]) -> Result<String, String> {
            Ok(String::new())
        }
        fn tcp_up(&self, _port: u16) -> bool {
            (self.up_from..self.up_until).contains(&self.clock.get())
        }
        fn now_secs(&self) -> u64 {
            self.clock.get()
        }
        fn sleep_secs(&self, n: u64) {
            self.clock.set(self.clock.get() + n);
        }
    }

    fn listener(up_from: u64, up_until: u64) -> Listener {
        Listener { up_from, up_until, clock: std::cell::Cell::new(0) }
    }

    // covers: UC-config-store-preflight-24
    #[test_id(tf0zxe4)]
    #[test]
    fn phase_4_waits_for_the_store_port_to_open_and_gives_up_exactly_at_the_bound() {
        let late = listener(3, u64::MAX);
        assert!(wait_for_tcp(&late, 3307, 10), "a port that opens inside the window passes");
        assert_eq!(late.clock.get(), 3, "and the wait stops the moment it opens");

        let never = listener(u64::MAX, u64::MAX);
        assert!(!wait_for_tcp(&never, 3307, 5), "positive control: a port that never opens is a timeout");
        assert_eq!(never.clock.get(), 5, "the wait is bounded by the timeout, not open-ended");
    }

    // covers: UC-config-store-preflight-24
    #[test_id(txo1mk5)]
    #[test]
    fn phase_4_waits_for_the_temporary_server_to_release_the_port_before_starting_the_unit() {
        let dying = listener(0, 2);
        assert!(wait_for_tcp_closed(&dying, 3307, 10), "a server that goes away inside the window clears the wait");
        assert_eq!(dying.clock.get(), 2);

        let stuck = listener(0, u64::MAX);
        assert!(!wait_for_tcp_closed(&stuck, 3307, 4), "a server that never releases the port is reported, bounded");
        assert_eq!(stuck.clock.get(), 4);
    }

    /// `spira-lc admin-store ping` answers once `ok_from` seconds have passed.
    struct Pinged {
        ok_from: u64,
        clock: std::cell::Cell<u64>,
        asked: std::cell::RefCell<Vec<String>>,
    }
    impl Deps for Pinged {
        fn tool(&self, name: &str, args: &[&str], _env: &[(&str, &str)]) -> Result<String, String> {
            self.asked.borrow_mut().push(format!("{name} {}", args.join(" ")));
            if self.clock.get() >= self.ok_from { Ok(String::new()) } else { Err("connecting: refused".into()) }
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

    #[test_id(tqpg3y7)]
    #[test]
    fn the_store_wait_asks_admin_store_ping_until_it_answers_and_gives_up_at_the_bound() {
        let pinged = |ok_from| Pinged { ok_from, clock: Default::default(), asked: Default::default() };
        let late = pinged(3);
        assert!(wait_for_store(&late, "lc", 10));
        assert_eq!(late.clock.get(), 3);
        assert!(late.asked.borrow().iter().all(|a| a == "lc admin-store ping"), "{:?}", late.asked.borrow());
        let never = pinged(u64::MAX);
        assert!(!wait_for_store(&never, "lc", 5));
        assert_eq!(never.clock.get(), 5);
    }

    /// Runs [`create_beads`] over `answers`, in order: its result, how often it ran, how often it dropped.
    fn creating(tries: u32, answers: &[(i32, &str)]) -> (Result<(), String>, usize, usize) {
        let mut ran = 0;
        let mut dropped = 0;
        let out = create_beads(
            &mut || {
                let (code, out) = answers[ran];
                ran += 1;
                (code, out.to_string())
            },
            &mut || dropped += 1,
            &mut |_| {},
            tries,
        );
        (out, ran, dropped)
    }

    const BEGAN: &str = "admin-create-beads: creating database d\nadmin-create-beads: lost";

    #[test_id(t0snudv)]
    #[test]
    fn a_creation_cut_after_it_began_is_dropped_and_retried_up_to_the_bound() {
        assert_eq!(creating(3, &[(2, BEGAN), (0, "")]), (Ok(()), 2, 1));
        let (out, ran, dropped) = creating(2, &[(2, BEGAN), (124, BEGAN)]);
        assert_eq!((ran, dropped), (2, 2));
        assert!(out.unwrap_err().contains("lost"));
    }

    #[test_id(tc2t2cd)]
    #[test]
    fn a_database_this_run_never_began_to_create_is_never_dropped() {
        let (out, ran, dropped) = creating(3, &[(3, "admin-create-beads: database d is a partial beads store")]);
        assert_eq!((ran, dropped), (1, 0), "a refusal is final");
        assert!(out.unwrap_err().contains("partial"));
        assert_eq!(creating(3, &[(2, "connecting: refused"), (0, "")]), (Ok(()), 2, 0));
    }

    #[test_id(t2g8lef)]
    #[test]
    fn the_bead_count_is_the_sql_counts_cell_or_unknown() {
        assert_eq!(bead_count(r#"[{"n":"12"}]"#), "12");
        for bad in ["", "[]", "Error 1146", r#"[{"x":"1"}]"#] {
            assert_eq!(bead_count(bad), "?", "{bad:?}");
        }
    }
}
