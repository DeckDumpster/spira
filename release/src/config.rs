//! Where things are: the releases directory, the run directory, how many releases to keep,
//! the systemd unit directory, and the host values units are rendered with.
//!
//! Resolved from flags, then THE ONE SOURCE OF CONFIG: the file `$SPIRA_TOML` names, read
//! through `spira_config::process::cfg`/`cfg_parse` (per Ryan 2026-10-05). No registered key
//! here carries an environment override or a literal default of its own any more — a key
//! `cfg` cannot resolve is a refusal naming it, never a guessed value. [`unit_dir_from_env`]
//! and [`Config::hotfix_alert_hours`] are the two exceptions: `SPIRA_UNIT_DIR` and
//! `SPIRA_HOTFIX_ALERT_HOURS` are not registered keys (no `spira/conf.d/` entry), so they
//! keep reading the raw environment exactly as before.

use spira_config::SpiraToml;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The environment a `Config` is resolved from — a map, so tests pass their own. Only ever
/// consulted for keys that are NOT registered config (`SPIRA_UNIT_DIR`, `HOME`,
/// `XDG_CONFIG_HOME`, `SPIRA_HOTFIX_ALERT_HOURS`, `DOLT`) — every registered key comes from
/// [`Registered`] instead.
pub type Env = BTreeMap<String, String>;

pub fn process_env() -> Env {
    std::env::vars().collect()
}

#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub releases: Option<PathBuf>,
    pub run: Option<PathBuf>,
    pub keep: Option<usize>,
}

/// Every registered config key [`Config::resolve`] reads through `cfg`/`cfg_parse`, already
/// resolved to a `String` — what [`Config::resolve_with`] (the pure half of resolution) takes
/// instead of touching `spira_config` itself. Tests build this directly (per Ryan 2026-10-05:
/// one source of config; pure logic takes config as arguments, never the per-process cache).
/// An empty field is exactly what `cfg` returns for a key `spira.toml` declares empty on
/// purpose (`SPIRA_DB`, `SPIRA_ALERT_GLOB`'s own siblings) — not a missing value.
#[derive(Debug, Clone, Default)]
pub struct Registered {
    pub releases: String,
    pub run: String,
    pub releases_keep: String,
    pub instance: String,
    pub home_repo: String,
    pub path: String,
    pub db: String,
    pub dolt_data: String,
    pub testdb_data: String,
    pub testdb_port: String,
    pub snap_stale_s: String,
    pub watchtower_start_timeout_s: String,
    pub sccache_dav_addr: String,
    pub lc_password_file: String,
    pub repo_map: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub releases: PathBuf,
    pub run: Option<PathBuf>,
    pub keep: usize,
    pub unit_dir: PathBuf,
    /// Non-registered ambient reads only: `DOLT`, `PATH` ([`Config::which`]) and
    /// `SPIRA_HOTFIX_ALERT_HOURS` ([`Config::hotfix_alert_hours`]) — never a registered key.
    env: Env,
    instance: String,
    home_repo: Option<String>,
    path_raw: String,
    db: String,
    dolt_data: String,
    testdb_data: String,
    testdb_port: String,
    snap_stale_s: String,
    watchtower_start_timeout_s: String,
    sccache_dav_addr: String,
    lc_password_file: String,
    repo_map: String,
    /// `spira.hotfix_alert_hours` from the host config document — `SPIRA_HOTFIX_ALERT_HOURS`
    /// is not a registered key, so this one field is still read from the typed document
    /// rather than `cfg_parse` (see [`Config::hotfix_alert_hours`]).
    hotfix_alert_hours_toml: Option<u32>,
}

/// Default hours a standing hotfix may run before `release status` alerts (DESIGN.md
/// "Hotfix: visibility") — `SPIRA_HOTFIX_ALERT_HOURS` is not a registered key, so this
/// crate-local default stands (nothing in `spira_config`'s registry owns it).
pub const DEFAULT_HOTFIX_ALERT_HOURS: u64 = 4;

fn nonempty(v: &str) -> Option<String> {
    (!v.is_empty()).then(|| v.to_string())
}

fn nonempty_opt(v: Option<&String>) -> Option<String> {
    v.filter(|s| !s.is_empty()).cloned()
}

/// The systemd user unit directory: `SPIRA_UNIT_DIR`, else `$XDG_CONFIG_HOME/systemd/user`,
/// else `$HOME/.config/systemd/user`. `SPIRA_UNIT_DIR` is not a registered config key (no
/// `spira/conf.d/` entry) — ambient environment, same as always. A standalone function (not
/// only a [`Config`] field) so a caller that needs only this — `release intake`, which has
/// nothing to do with `spira-releases/` at all — is never coupled to [`Config::resolve`]'s own
/// requirement that the releases directory resolve too (DESIGN.md "intake": intake's own
/// config is orthogonal to a release's).
pub fn unit_dir_from_env(env: &Env) -> Result<PathBuf, String> {
    nonempty_opt(env.get("SPIRA_UNIT_DIR"))
        .map(PathBuf::from)
        .or_else(|| nonempty_opt(env.get("XDG_CONFIG_HOME")).map(|x| PathBuf::from(x).join("systemd/user")))
        .or_else(|| nonempty_opt(env.get("HOME")).map(|h| PathBuf::from(h).join(".config/systemd/user")))
        .ok_or_else(|| "no systemd unit directory: set SPIRA_UNIT_DIR, XDG_CONFIG_HOME or HOME".to_string())
}

impl Config {
    /// Resolve from `flags`, `env` (non-registered ambient reads only) and the one source of
    /// config: every registered key this crate needs, through `spira_config::process::cfg`/
    /// `cfg_parse`. `Err` on the first key that cannot resolve, naming it — never a default.
    pub fn resolve(flags: &Flags, env: &Env) -> Result<Config, String> {
        use spira_config::process::cfg;
        let doc = spira_config::discover(None).map(|p| spira_config::load(&p)).transpose()?;
        let reg = Registered {
            releases: cfg("SPIRA_RELEASES")?,
            run: cfg("SPIRA_RUN")?,
            releases_keep: cfg("SPIRA_RELEASES_KEEP")?,
            instance: cfg("SPIRA_INSTANCE")?,
            home_repo: cfg("SPIRA_HOME_REPO")?,
            path: cfg("SPIRA_PATH")?,
            db: cfg("SPIRA_DB")?,
            dolt_data: cfg("SPIRA_DOLT_DATA")?,
            testdb_data: cfg("SPIRA_TESTDB_DATA")?,
            testdb_port: cfg("SPIRA_TESTDB_PORT")?,
            snap_stale_s: cfg("SPIRA_SNAP_STALE_S")?,
            watchtower_start_timeout_s: cfg("SPIRA_WATCHTOWER_START_TIMEOUT_S")?,
            sccache_dav_addr: cfg("SPIRA_SCCACHE_DAV_ADDR")?,
            lc_password_file: cfg("SPIRA_LC_PASSWORD_FILE")?,
            repo_map: cfg("SPIRA_REPO_MAP")?,
        };
        Config::resolve_with(flags, env, doc, &reg)
    }

    /// The pure half of resolution: `flags` override `reg` (every registered key, already
    /// resolved by the caller — see [`Config::resolve`]); `env` is consulted only for the
    /// handful of non-registered ambient keys; `doc` only for `spira.hotfix_alert_hours`
    /// (also not registered). No `spira_config::process` call happens in here, so a test can
    /// drive every path by constructing `Registered`/`Env`/`doc` directly, independent of the
    /// per-process config cache.
    pub fn resolve_with(flags: &Flags, env: &Env, doc: Option<SpiraToml>, reg: &Registered) -> Result<Config, String> {
        let releases = flags
            .releases
            .clone()
            .or_else(|| nonempty(&reg.releases).map(PathBuf::from))
            .ok_or("no releases directory: pass --releases, or set spira.releases (SPIRA_RELEASES) in spira.toml")?;
        let run = flags.run.clone().or_else(|| nonempty(&reg.run).map(PathBuf::from));
        let keep = match flags.keep {
            Some(k) => k,
            None => reg
                .releases_keep
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|k| *k >= 1)
                .ok_or_else(|| format!("SPIRA_RELEASES_KEEP must be a whole number >= 1, got {:?}", reg.releases_keep))?,
        };
        let unit_dir = unit_dir_from_env(env)?;
        let hotfix_alert_hours_toml = doc.and_then(|t| t.spira).and_then(|s| s.hotfix_alert_hours);
        Ok(Config {
            releases,
            run,
            keep,
            unit_dir,
            env: env.clone(),
            instance: reg.instance.clone(),
            home_repo: nonempty(&reg.home_repo),
            path_raw: reg.path.clone(),
            db: reg.db.clone(),
            dolt_data: reg.dolt_data.clone(),
            testdb_data: reg.testdb_data.clone(),
            testdb_port: reg.testdb_port.clone(),
            snap_stale_s: reg.snap_stale_s.clone(),
            watchtower_start_timeout_s: reg.watchtower_start_timeout_s.clone(),
            sccache_dav_addr: reg.sccache_dav_addr.clone(),
            lc_password_file: reg.lc_password_file.clone(),
            repo_map: reg.repo_map.clone(),
            hotfix_alert_hours_toml,
        })
    }

    /// `$SPIRA_RUN/release`, where the history and the hotfix record live.
    pub fn state_dir(&self) -> Result<PathBuf, String> {
        self.run
            .as_ref()
            .map(|r| r.join("release"))
            .ok_or_else(|| "no run directory: pass --run, or set spira.run (SPIRA_RUN) in spira.toml".to_string())
    }

    /// A non-registered ambient key (`DOLT`, `PATH` via [`Config::which`], or
    /// `SPIRA_HOTFIX_ALERT_HOURS`) — never a registered config key, which always comes
    /// through a [`Config`] field resolved at construction instead.
    fn env(&self, key: &str) -> Option<String> {
        nonempty_opt(self.env.get(key))
    }

    /// The instance suffix of per-instance unit names (`SPIRA_INSTANCE`).
    pub fn instance(&self) -> String {
        self.instance.clone()
    }

    pub fn home_repo(&self) -> Option<String> {
        self.home_repo.clone()
    }

    /// The box's own tool-directory tail (`SPIRA_PATH`) — the same value
    /// [`host_values`](Config::host_values) renders into `SPIRA_PATH_TAIL`, and what a child
    /// process this crate spawns *from* a release (verify's pre-activate, sp-vrn3v) appends
    /// to [`spira_config::release_path_with_tail`] after that release's own directories.
    /// `Err`, naming the offending entry, when a segment resolves inside a release or a
    /// checkout (sp-c7b85).
    pub fn path_tail(&self) -> Result<String, String> {
        let refusals = spira_config::tail_refusals(&self.path_raw);
        if !refusals.is_empty() {
            return Err(refusals.join("; "));
        }
        Ok(self.path_raw.clone())
    }

    /// The host values unit templates are rendered with (DESIGN.md "Render"): every
    /// registered key as `spira_config::process::cfg` already resolved it, plus `DOLT`
    /// (not registered — the ambient environment, else a `PATH` lookup). A key with no
    /// value maps to the empty string, which the renderer refuses when a template uses it.
    ///
    /// `Err` only for `SPIRA_PATH_TAIL` (sp-c7b85): see [`Config::path_tail`].
    pub fn host_values(&self) -> Result<BTreeMap<String, String>, String> {
        let mut m = BTreeMap::new();
        m.insert("SPIRA_RUN".into(), self.run.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        m.insert("SPIRA_DB".into(), self.db.clone());
        m.insert("SPIRA_INSTANCE".into(), self.instance());
        m.insert("SPIRA_DOLT_DATA".into(), self.dolt_data.clone());
        m.insert("SPIRA_TESTDB_DATA".into(), self.testdb_data.clone());
        m.insert("SPIRA_TESTDB_PORT".into(), self.testdb_port.clone());
        m.insert("SPIRA_SNAP_STALE_S".into(), self.snap_stale_s.clone());
        m.insert("SPIRA_WATCHTOWER_START_TIMEOUT_S".into(), self.watchtower_start_timeout_s.clone());
        // DOLT is not a registered config key (spira/conf.d) — ambient env, else a PATH lookup.
        m.insert("DOLT".into(), self.env("DOLT").or_else(|| self.which("dolt")).unwrap_or_default());
        m.insert(crate::units::SCCACHE_DAV_ADDR_KEY.into(), self.sccache_dav_addr.clone());
        m.insert("SPIRA_LC_PASSWORD_FILE".into(), self.lc_password_file.clone());
        m.insert("SPIRA_REPO_MAP".into(), self.repo_map.clone());
        let tail = self.path_tail()?;
        m.insert("SPIRA_PATH_TAIL".into(), if tail.is_empty() { String::new() } else { format!(":{tail}") });
        Ok(m)
    }

    /// Hours a standing hotfix may run before `release status` emits its ALERT line
    /// (DESIGN.md "Hotfix: visibility"): `SPIRA_HOTFIX_ALERT_HOURS`, else
    /// `spira.hotfix_alert_hours`, else [`DEFAULT_HOTFIX_ALERT_HOURS`]. Neither key is
    /// registered (no `spira/conf.d/` entry), so this is the one place in this crate that
    /// still reads the raw environment/document rather than `cfg`/`cfg_parse`. `Err` on a
    /// non-numeric or zero override, naming it, rather than silently alerting on every pass
    /// or never at all.
    pub fn hotfix_alert_hours(&self) -> Result<u64, String> {
        if let Some(s) = self.env("SPIRA_HOTFIX_ALERT_HOURS") {
            return s
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|h| *h >= 1)
                .ok_or_else(|| format!("SPIRA_HOTFIX_ALERT_HOURS must be a whole number >= 1, got {s:?}"));
        }
        match self.hotfix_alert_hours_toml {
            None => Ok(DEFAULT_HOTFIX_ALERT_HOURS),
            Some(0) => Err("spira.hotfix_alert_hours must be >= 1, got 0".to_string()),
            Some(h) => Ok(u64::from(h)),
        }
    }

    /// `DOLT` is not a registered config key — a `PATH` lookup, same as any other program
    /// name a caller never configures.
    fn which(&self, prog: &str) -> Option<String> {
        let path = self.env("PATH")?;
        path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(prog)).find(|p| crate::fsutil::is_executable(p)).map(|p| p.display().to_string())
    }
}
