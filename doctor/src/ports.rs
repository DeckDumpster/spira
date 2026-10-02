//! Everything doctor.sh's 14 checks need from the world, as a trait (DESIGN.md §4).
//! `real.rs` implements it against the host: a one-shot `conf.sh`+`lib.sh` seam (the same
//! technique `skew`, `gate-check` and `queue` use) captures every derived config value
//! once at startup, then each check reads it plus does its own read-only probe (systemctl,
//! `bd`, a TCP connect, a file's mtime). `tests.rs`'s `Fake` returns canned data per check.

use std::path::{Path, PathBuf};

/// `dolt_mode` / `dolt_server_host` / `dolt_server_port` from a store's `metadata.json`.
#[derive(Debug, Clone, Default)]
pub struct StoreMeta {
    pub dolt_mode: Option<String>,
    pub dolt_server_host: String,
    pub dolt_server_port: Option<u16>,
}

pub trait World {
    /// A value `conf.sh` derived (or a bare process env var it passed through), captured
    /// once at startup under `SPIRA_DOCTOR=1` exactly as doctor.sh itself sourced conf.sh.
    fn env(&self, k: &str) -> Option<String>;

    /// `command -v <name>` on the launcher's PATH; the resolved path when found.
    fn which(&self, name: &str) -> Option<PathBuf>;
    /// `[ -x <p> ]` — a literal executable-file test, never a PATH search (bash's own `go`
    /// candidate loop uses this, not `command -v`, for every candidate but the last).
    fn is_executable_file(&self, p: &str) -> bool;

    /// `spira_deps_list release` (lib.sh seam) — every Spira binary deps.toml's `release`
    /// tier declares.
    fn deps_list_release(&self) -> Vec<String>;

    /// `release status` -> combined stdout.
    fn release_status(&self) -> String;

    /// `spira-config validate <toml>` -> Ok, or Err(combined output) when it fails to run
    /// or the document does not validate.
    fn spira_config_validate(&self, toml_path: &Path) -> Result<(), String>;

    /// `spira-config migrate <toml>` (sp-oppza) -> its combined output, or empty when it had
    /// nothing to migrate. Its own exit code is never read — same as doctor.sh's `mig="$(...
    /// 2>&1)"` — `validate`, called right after, is what this section actually gates on.
    fn spira_config_migrate(&self, toml_path: &Path) -> String;

    /// `find <dir> -maxdepth 2 -name '*.md'`, each path relative to `dir`.
    fn find_md_files(&self, dir: &Path) -> Vec<String>;

    /// `overrides.sh doctor` -> Ok on exit 0, Err(its stdout, one problem per line) otherwise.
    fn overrides_doctor(&self) -> Result<String, String>;

    // ---- lib.sh repository-map seam (never re-derived; see skew/DESIGN.md §2 for the same rule) ----
    fn repo_names(&self) -> Vec<String>;
    fn repo_field(&self, name: &str, field: &str) -> Option<String>;
    fn dir_has_cargo_toml_within(&self, path: &Path) -> bool; // maxdepth 2
    /// `gate --home <home> --definition <name>` -> Ok(the gate string) or Err(why it could
    /// not resolve — printed as `<unresolved: ...>`, matching bash's `||` fallback).
    fn gate_definition(&self, home: &Path, name: &str) -> Result<String, String>;

    // ---- store ----
    fn dir_exists(&self, p: &Path) -> bool;
    fn file_exists(&self, p: &Path) -> bool;
    /// `timeout 60 bd -C <db> list --limit 1 --json` -> Ok(json) or Err(combined output).
    fn bd_list(&self, db: &Path, timeout_secs: u64) -> Result<String, String>;
    fn read_store_meta(&self, metadata_json: &Path) -> Option<StoreMeta>;
    fn systemd_user_is_active(&self, unit: &str) -> bool;
    fn tcp_connect(&self, host: &str, port: u16) -> bool;

    // ---- events probe ----
    /// The first bead id `bd -C <db> list --limit 1 --json` returns, or None.
    fn bd_first_id(&self, db: &Path) -> Option<String>;
    /// `_bump_write_event_try <id> <event-type> <actor>` (lib.sh seam) -> whether it wrote.
    fn bump_write_event_try(&self, id: &str, etype: &str, actor: &str) -> bool;
    /// `_counter_events_query <id> <event-type>` (lib.sh seam) -> a decimal count, or "?" on
    /// a read failure — matching bash's own "never choke on a non-digit" guard.
    fn counter_events_query(&self, id: &str, etype: &str) -> String;

    // ---- systemd units ----
    /// `systemctl --user list-units --state=failed --no-legend --plain <pattern>`.
    /// Err only when the query itself could not run; Ok(unit names) otherwise (empty = none).
    fn systemd_failed_units(&self, pattern: &str) -> Result<Vec<String>, String>;
    /// `watchd manifest` (the Rust binary, was `watchd.sh`) -> Ok(its stdout) or
    /// Err(query failed).
    fn watchd_manifest(&self) -> Result<String, String>;
    /// `systemctl --user list-unit-files --no-legend --state=enabled <pattern>`. Ok(unit
    /// names); Err(first output line) only when the manager itself could not be reached
    /// (distinguishing "no match" — silent, exit 1, Ok(vec![]) — from a real query fault,
    /// which DOES print something even on a non-zero exit).
    fn systemd_enabled_unit_files(&self, pattern: &str) -> Result<Vec<String>, String>;
    /// `spira_unit <kind> <subkind>` (lib.sh seam) — a rendered unit name for a hint line.
    fn spira_unit(&self, kind: &str, subkind: &str) -> String;

    // ---- snapshot freshness ----
    /// Seconds since `p`'s mtime, or None if it does not exist / cannot be read.
    fn file_age_secs(&self, p: &Path) -> Option<u64>;

    // ---- operator channel ----
    fn bd_version(&self) -> Option<String>;
    fn arch(&self) -> String;
    /// `spira_bin_purpose <name>` (lib.sh seam) — one sentence on what an absent binary costs.
    fn spira_bin_purpose(&self, name: &str) -> String;

    // ---- concierge singleton ----
    /// `bash <concierge.sh> _stray-holders` -> PIDs, one per line, empty when none.
    fn concierge_stray_holders(&self, concierge_sh: &Path) -> Vec<String>;

    // ---- compilation cache ----
    /// `sccache --help`'s stdout, verbatim; `None` if `sccache` could not be run at all
    /// (absent is reported separately via [`World::which`] — this is "it ran, here is what
    /// it says about itself"). sp-xjnzl: a binary built `--no-default-features` compiles and
    /// runs fine but silently drops every remote backend, so presence alone (`which`) is not
    /// a health check — the "Enabled features:" block this prints is the only place that
    /// says so.
    fn sccache_help(&self) -> Option<String>;

    /// `sccache --show-stats`'s `Cache location` line, verbatim (trimmed), or `None` if the
    /// binary could not be run at all. sp-xtdqi: querying a server that is already running
    /// only reads its socket — the daemon's own backend was fixed at ITS spawn time, which is
    /// exactly the fact this check exists to surface (a config that names a store is not the
    /// same question as a server that is actually on it).
    fn sccache_show_stats(&self) -> Option<String>;

    /// `SPIRA_SCCACHE_DAV_ADDR`, resolved in-process the same way `spira_config::build`
    /// resolves it for a real build (law-a-binary-resolves-the-config-it-reads): env first,
    /// then the host config document. sp-xtdqi-3: this is NOT the same as [`World::env`] —
    /// that is `conf.sh`'s own bash capture, and `conf.sh` carries no statement at all for a
    /// NO-DEFAULT key like this one, so a box that genuinely configured it (in the host config,
    /// never exported) was invisible to the bash capture and this check passed blind.
    fn sccache_dav_addr(&self) -> Option<String>;

    fn out(&self, s: &str);
}
