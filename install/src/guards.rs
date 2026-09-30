//! The root installer's pre-flight guards: path collisions between instances, the five
//! conflict predicates (phase 0.5), the `SPIRA_PROD` git/bootstrap guards, and the database
//! git guard. Each is a pure function over injected state (a `/proc` root, a TCP probe, a set
//! of other config files already read) rather than the real filesystem or network, matching
//! docs/test-plan/instance-lifecycle.md's own extraction plan for these checks.

use std::collections::BTreeMap;
use std::path::Path;

/// A conflict guard's refusal: an exit code (always 5, kept explicit because
/// `_conflict_report` in the bash always used one), a one-line reason and a remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub code: i32,
    pub message: String,
    pub remedy: String,
}

impl Conflict {
    fn new(message: impl Into<String>, remedy: impl Into<String>) -> Conflict {
        Conflict { code: 5, message: message.into(), remedy: remedy.into() }
    }
}

// ---------------------------------------------------------------------------------------
// _check_path_collisions
// ---------------------------------------------------------------------------------------

/// One other instance's config file, already parsed into its `SPIRA_INSTANCE` (default
/// "prod") and whichever of the five watched keys it sets explicitly. Parsing the raw file
/// text (trim, strip `#` comments and quotes, `~`/`$HOME` expansion) is [`parse_conf_keys`];
/// kept separate so a test can hand this struct in directly.
#[derive(Debug, Clone, Default)]
pub struct OtherConf {
    pub file: String,
    pub instance: String,
    pub values: BTreeMap<String, String>,
}

pub const COLLISION_KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_DB", "SPIRA_PROD", "SPIRA_DOLT_DATA", "SPIRA_TESTDB_PORT"];

/// Parse one `*.conf` file's text into an [`OtherConf`] (units.sh's `_check_path_collisions`
/// inline parser, in Rust): `KEY=VALUE` lines only, `#`-comments and blank lines skipped,
/// surrounding quotes stripped, a leading `~` or `$HOME`/`${HOME}` expanded against `home`.
pub fn parse_conf_keys(file: &str, text: &str, home: &str) -> OtherConf {
    let mut c = OtherConf { file: file.to_string(), instance: "prod".to_string(), values: BTreeMap::new() };
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else { continue };
        let key = key.trim().to_string();
        let mut val = val.trim().to_string();
        if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2) || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2) {
            val = val[1..val.len() - 1].to_string();
        }
        if val == "~" {
            val = home.to_string();
        } else if let Some(rest) = val.strip_prefix("~/") {
            val = format!("{home}/{rest}");
        }
        val = val.replace("$HOME", home).replace("${HOME}", home);
        if key == "SPIRA_INSTANCE" {
            c.instance = val;
        } else if COLLISION_KEYS.contains(&key.as_str()) {
            c.values.insert(key, val);
        }
    }
    c
}

/// A different instance's config setting one of [`COLLISION_KEYS`] to the same value this
/// instance resolved. Checked before any directory is created or unit written, so a refusal
/// leaves the box exactly as it was.
pub fn check_path_collisions(this_instance: &str, this: &BTreeMap<&str, &str>, others: &[OtherConf]) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    for other in others {
        if other.instance == this_instance {
            continue;
        }
        for key in COLLISION_KEYS {
            let (Some(their_val), Some(our_val)) = (other.values.get(*key), this.get(key)) else { continue };
            if their_val == our_val {
                problems.push(format!("{key} collides with instance {} ({}): both set to {}", other.instance, other.file, our_val));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

// ---------------------------------------------------------------------------------------
// Conflict predicates 1–5 (root install.sh phase 0.5)
// ---------------------------------------------------------------------------------------

/// Conflict 1: a foreign harness copy owns this instance's unit names. `installed_exec_dir`
/// is the directory an already-installed `spira-sentinel*.service`'s `ExecStart=` resolves
/// to (`None` when no such unit is installed), already canonicalised by the caller; `our_dir`
/// is this installation's own equivalent, also canonicalised.
pub fn conflict_foreign(instance: &str, installed_exec_dir: Option<&str>, our_dir: &str) -> Result<(), Conflict> {
    let Some(theirs) = installed_exec_dir else { return Ok(()) };
    if theirs == our_dir {
        return Ok(());
    }
    Err(Conflict::new(
        format!("installed units for instance '{instance}' exec from {theirs} (not {our_dir})"),
        "uninstall the other copy first, or re-run with SPIRA_INSTALL_CONFLICT_CONSIDERED=1 to repoint the units",
    ))
}

/// Conflict 2: a live aeon under this installation. `cmdlines` stands in for scanning
/// `/proc/*/cmdline` (never `pgrep -f`, which matches the caller's own command line —
/// law-a-pattern-match-is-not-an-identity-check): each entry is `(pid, cmdline)`.
pub fn conflict_aeon<'a>(home: &str, cmdlines: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(), Conflict> {
    let needle = format!("{home}/aeon.sh");
    for (pid, cmd) in cmdlines {
        if cmd.split('\0').any(|a| a == needle) {
            return Err(Conflict::new(format!("live aeon running under this installation (pid {pid})"), format!("wait for it to finish, or run: {home}/world.sh stop; then re-run install")));
        }
    }
    Ok(())
}

/// Conflict 3: the landing gate's tree lock is held. `held` stands in for `flock -n <lock>
/// true` failing.
pub fn conflict_lock(lock: &str, held: bool) -> Result<(), Conflict> {
    if held {
        Err(Conflict::new(format!("landing pass in flight — gate tree lock is held at {lock}"), "wait for the landing pass to complete, then re-run install"))
    } else {
        Ok(())
    }
}

/// Conflict 4a: the instance argument disagrees with the existing config's `SPIRA_INSTANCE`.
pub fn conflict_instance_arg(instance: &str, conf_instance: Option<&str>, conf_file: &str) -> Result<(), Conflict> {
    match conf_instance {
        Some(v) if !v.is_empty() && v != instance => Err(Conflict::new(
            format!("instance argument '{instance}' disagrees with config SPIRA_INSTANCE='{v}'"),
            format!("re-run without an instance argument, or edit SPIRA_INSTANCE in {conf_file}"),
        )),
        _ => Ok(()),
    }
}

/// Conflict 4b: another installed instance's sentinel already points its `SPIRA_RUN` at this
/// one's run directory. `other_sentinels` is `(unit_basename, its StandardOutput run dir)`,
/// already resolved and canonicalised by the caller.
pub fn conflict_instance_run<'a>(run_dir: &str, our_unit: &str, other_sentinels: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(), Conflict> {
    for (unit, other_run) in other_sentinels {
        if unit == our_unit {
            continue;
        }
        if other_run == run_dir {
            let other_instance = unit.strip_suffix(".service").and_then(|s| s.rsplit('-').next()).unwrap_or(unit);
            return Err(Conflict::new(format!("instance '{other_instance}' units ({unit}) already point at SPIRA_RUN={run_dir}"), "use a different SPIRA_RUN, or uninstall the other instance first"));
        }
    }
    Ok(())
}

/// Conflict 5: a Dolt `sql-server` is listening on the configured port with a different
/// `data_dir`. `listening` stands in for the TCP probe; `servers` is `(cmdline, data_dir)`
/// for every `sql-server` process found under `/proc` (data_dir already extracted from
/// `--data-dir` or a `--config` file by the caller, and canonicalised).
pub fn conflict_dolt<'a>(dolt_data: &str, port: u16, listening: bool, servers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(), Conflict> {
    if dolt_data.is_empty() || !listening {
        return Ok(());
    }
    for (_cmd, data_dir) in servers {
        if data_dir != dolt_data {
            return Err(Conflict::new(format!("Dolt server listening on port {port} is serving '{data_dir}' (not {dolt_data})"), "stop the other Dolt server or set SPIRA_DOLT_DATA to match its data directory"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// SPIRA_PROD / SPIRA_DB guards
// ---------------------------------------------------------------------------------------

/// `_configure_prod_guard`: `CONFIGURE_PROD` must be the harness subdir (it holds `conf.sh`),
/// not the clone root.
pub fn configure_prod_guard(path: &str, has_conf_sh: bool) -> Result<(), String> {
    if path.is_empty() || has_conf_sh {
        return Ok(());
    }
    Err(format!("CONFIGURE_PROD ({path}) does not contain conf.sh — set CONFIGURE_PROD to the harness subdir: {path}/spira"))
}

/// `_prod_guard`: `SPIRA_PROD` must not be a git checkout — the release model requires it to
/// resolve through `<releases>/current`, which `release install-tarball` swaps atomically.
pub fn prod_guard(path: &str, is_git_checkout: bool, override_set: bool, releases: &str) -> Result<(), String> {
    if override_set || !is_git_checkout {
        return Ok(());
    }
    Err(format!(
        "REFUSING — SPIRA_PROD ({path}) is a git checkout. The release model requires SPIRA_PROD to resolve through {releases}/current \
         (the symlink release install-tarball swaps on each deploy). Activate a release tarball first: release install-tarball <tarball>. \
         Override: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1"
    ))
}

/// `_bootstrap_decision`: a missing `SPIRA_PROD`'s parent must resolve under `releases`
/// (phase 4 may bootstrap `releases/bootstrap` from the installing clone); otherwise refuse,
/// naming `release install-tarball`.
pub fn bootstrap_decision(prod: &str, releases_canon: &str) -> Result<(), String> {
    let parent = Path::new(prod).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    if parent == releases_canon || parent.starts_with(&format!("{releases_canon}/")) {
        return Ok(());
    }
    Err(format!("SPIRA_PROD ({prod}) does not exist — activate a release first: release install-tarball <tarball>"))
}

/// `_db_git_guard`: refuse a database inside a git checkout (any `.git` in a directory
/// above it), or whose own repository has a remote. The database's own repository with no
/// remote is allowed — `bd init` creates exactly that.
pub fn db_git_guard(db: &str, own_repo_remotes: &[String], ancestor_git_dir: Option<&str>) -> Result<(), String> {
    if db.is_empty() {
        return Ok(());
    }
    if !own_repo_remotes.is_empty() {
        return Err(format!(
            "REFUSING database at {db} — its own git repository has a remote ({}). A database with a git remote is one push from publishing every bead body. \
             Remove the remote (git -C {db} remote remove <name>) or move SPIRA_DB.",
            own_repo_remotes.join(" ")
        ));
    }
    if let Some(dir) = ancestor_git_dir {
        return Err(format!(
            "REFUSING database at {db} — it is inside a git checkout ({dir}). A git-tracked database accumulates internal notes and is one \"git add -A\" \
             from publishing every bead body. Set SPIRA_DB outside any git checkout in spira.conf."
        ));
    }
    Ok(())
}

/// `_seed_when`: when phase 3 should run `seed.sh` — "now" unless the database is
/// server-mode and its server is not yet up, in which case it defers to phase 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedWhen {
    Now,
    Defer,
}

pub fn seed_when(fresh: bool, server_mode: bool, server_up: bool) -> SeedWhen {
    if fresh || !server_mode || server_up {
        SeedWhen::Now
    } else {
        SeedWhen::Defer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_key_parsing_strips_quotes_and_expands_home() {
        let c = parse_conf_keys("other.conf", "# comment\nSPIRA_INSTANCE=test\nSPIRA_RUN=\"~/run\"\nSPIRA_DB='$HOME/db'\nIGNORED=1\n", "/home/ryan");
        assert_eq!(c.instance, "test");
        assert_eq!(c.values["SPIRA_RUN"], "/home/ryan/run");
        assert_eq!(c.values["SPIRA_DB"], "/home/ryan/db");
        assert!(!c.values.contains_key("IGNORED"));
    }

    #[test]
    fn a_shared_run_dir_across_instances_collides() {
        let this: BTreeMap<&str, &str> = BTreeMap::from([("SPIRA_RUN", "/run/prod")]);
        let other = OtherConf { file: "test.conf".into(), instance: "test".into(), values: BTreeMap::from([("SPIRA_RUN".to_string(), "/run/prod".to_string())]) };
        assert!(check_path_collisions("prod", &this, &[other]).is_err());
    }

    #[test]
    fn the_same_instance_never_collides_with_itself() {
        let this: BTreeMap<&str, &str> = BTreeMap::from([("SPIRA_RUN", "/run/prod")]);
        let other = OtherConf { file: "prod.conf".into(), instance: "prod".into(), values: BTreeMap::from([("SPIRA_RUN".to_string(), "/run/prod".to_string())]) };
        assert!(check_path_collisions("prod", &this, &[other]).is_ok());
    }

    #[test]
    fn conflict_foreign_is_clear_when_no_unit_is_installed_or_it_matches() {
        assert!(conflict_foreign("prod", None, "/home").is_ok());
        assert!(conflict_foreign("prod", Some("/home"), "/home").is_ok());
        assert!(conflict_foreign("prod", Some("/other"), "/home").is_err());
    }

    #[test]
    fn conflict_aeon_matches_the_full_argv_entry_only() {
        assert!(conflict_aeon("/home", [("123", "bash\0/home/aeon.sh\0--foo")]).is_err());
        // A caller whose OWN cmdline merely mentions the path (e.g. this grep's argv) must
        // not match — law-a-pattern-match-is-not-an-identity-check.
        assert!(conflict_aeon("/home", [("999", "grep\0-alFf\0/dev/stdin\0looking for /home/aeon.sh")]).is_ok());
    }

    #[test]
    fn conflict_dolt_is_clear_unless_a_different_data_dir_is_being_served() {
        assert!(conflict_dolt("/dolt", 3307, false, []).is_ok());
        assert!(conflict_dolt("/dolt", 3307, true, [("sql-server", "/dolt")]).is_ok());
        assert!(conflict_dolt("/dolt", 3307, true, [("sql-server", "/other")]).is_err());
    }

    #[test]
    fn bootstrap_decision_requires_prods_parent_under_releases() {
        assert!(bootstrap_decision("/releases/bootstrap", "/releases").is_ok());
        assert!(bootstrap_decision("/elsewhere/spira", "/releases").is_err());
    }

    #[test]
    fn db_git_guard_refuses_a_remote_before_an_ancestor_checkout() {
        assert!(db_git_guard("/db", &["origin".to_string()], Some("/checkout")).unwrap_err().contains("remote"));
        assert!(db_git_guard("/db", &[], Some("/checkout")).unwrap_err().contains("checkout"));
        assert!(db_git_guard("/db", &[], None).is_ok());
    }

    #[test]
    fn seed_when_defers_only_for_a_down_server_mode_database() {
        assert_eq!(seed_when(true, true, false), SeedWhen::Now); // fresh always seeds now
        assert_eq!(seed_when(false, false, false), SeedWhen::Now); // embedded mode
        assert_eq!(seed_when(false, true, true), SeedWhen::Now); // server already up
        assert_eq!(seed_when(false, true, false), SeedWhen::Defer);
    }
}
