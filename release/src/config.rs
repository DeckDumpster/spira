//! Where things are: the releases directory, the run directory, how many releases to keep,
//! the systemd unit directory, and the host values units are rendered with.
//!
//! Resolved from flags, then the environment, then the host config document, which only
//! `spira-config` finds and reads (`spira_config::discover`). A root nothing resolves is a
//! refusal naming what to set; there is no built-in path.

use spira_config::{SpiraSection, SpiraToml};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The environment a `Config` is resolved from — a map, so tests pass their own.
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

#[derive(Debug, Clone)]
pub struct Config {
    pub releases: PathBuf,
    pub run: Option<PathBuf>,
    pub keep: usize,
    pub unit_dir: PathBuf,
    env: Env,
    spira: SpiraSection,
}

/// Default number of releases prune keeps (design: about 65 MB each).
pub const DEFAULT_KEEP: usize = 5;

fn nonempty(v: Option<&String>) -> Option<String> {
    v.filter(|s| !s.is_empty()).cloned()
}

impl Config {
    /// Resolve from `flags`, `env` and the host config document `spira-config` discovers.
    pub fn resolve(flags: &Flags, env: &Env) -> Result<Config, String> {
        let doc = spira_config::discover(None).map(|p| spira_config::load(&p)).transpose()?;
        Config::resolve_with(flags, env, doc)
    }

    pub fn resolve_with(flags: &Flags, env: &Env, doc: Option<SpiraToml>) -> Result<Config, String> {
        let spira = doc.and_then(|t| t.spira).unwrap_or_default();
        let releases = flags
            .releases
            .clone()
            .or_else(|| nonempty(env.get("SPIRA_RELEASES")).map(PathBuf::from))
            .or_else(|| spira.releases.clone().filter(|s| !s.is_empty()).map(PathBuf::from))
            .or_else(|| spira.workspaces.clone().filter(|s| !s.is_empty()).map(|w| PathBuf::from(w).join("spira-releases")))
            .ok_or("no releases directory: pass --releases, or set SPIRA_RELEASES, spira.releases or spira.workspaces")?;
        let run = flags
            .run
            .clone()
            .or_else(|| nonempty(env.get("SPIRA_RUN")).map(PathBuf::from))
            .or_else(|| spira.run.clone().filter(|s| !s.is_empty()).map(PathBuf::from));
        let keep_src = flags
            .keep
            .map(|k| k.to_string())
            .or_else(|| nonempty(env.get("SPIRA_RELEASES_KEEP")))
            .or_else(|| spira.releases_keep.clone().filter(|s| !s.is_empty()));
        let keep = match keep_src {
            None => DEFAULT_KEEP,
            Some(s) => s.trim().parse::<usize>().ok().filter(|k| *k >= 1).ok_or_else(|| format!("releases_keep must be a whole number >= 1, got {s:?}"))?,
        };
        let unit_dir = nonempty(env.get("SPIRA_UNIT_DIR"))
            .map(PathBuf::from)
            .or_else(|| nonempty(env.get("XDG_CONFIG_HOME")).map(|x| PathBuf::from(x).join("systemd/user")))
            .or_else(|| nonempty(env.get("HOME")).map(|h| PathBuf::from(h).join(".config/systemd/user")))
            .ok_or("no systemd unit directory: set SPIRA_UNIT_DIR, XDG_CONFIG_HOME or HOME")?;
        Ok(Config { releases, run, keep, unit_dir, env: env.clone(), spira })
    }

    /// `$SPIRA_RUN/release`, where the history and the hotfix record live.
    pub fn state_dir(&self) -> Result<PathBuf, String> {
        self.run
            .as_ref()
            .map(|r| r.join("release"))
            .ok_or_else(|| "no run directory: pass --run, or set SPIRA_RUN or spira.run".to_string())
    }

    pub fn env(&self, key: &str) -> Option<String> {
        nonempty(self.env.get(key))
    }

    /// The instance suffix of per-instance unit names.
    pub fn instance(&self) -> String {
        self.env("SPIRA_INSTANCE").or_else(|| self.spira.instance.clone().filter(|s| !s.is_empty())).unwrap_or_else(|| "prod".into())
    }

    pub fn home_repo(&self) -> Option<String> {
        self.spira.home_repo.clone().filter(|s| !s.is_empty())
    }

    /// The host values unit templates are rendered with (DESIGN.md "Render"): the
    /// environment, else the host config, else conf.sh's own default. A key with no value maps
    /// to the empty string, which the renderer refuses when a template uses it.
    pub fn host_values(&self) -> BTreeMap<String, String> {
        let s = &self.spira;
        let pick = |k: &str, t: Option<&String>, d: Option<String>| -> String {
            self.env(k).or_else(|| t.filter(|v| !v.is_empty()).cloned()).or(d).unwrap_or_default()
        };
        let ws = s.workspaces.clone().filter(|v| !v.is_empty());
        let mut m = BTreeMap::new();
        m.insert("SPIRA_RUN".into(), self.run.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        m.insert("SPIRA_DB".into(), pick("SPIRA_DB", s.db.as_ref(), None));
        m.insert("SPIRA_INSTANCE".into(), self.instance());
        m.insert("SPIRA_DOLT_DATA".into(), pick("SPIRA_DOLT_DATA", s.dolt_data.as_ref(), None));
        m.insert("SPIRA_TESTDB_DATA".into(), pick("SPIRA_TESTDB_DATA", None, ws.map(|w| format!("{w}/beads-test"))));
        m.insert("SPIRA_TESTDB_PORT".into(), pick("SPIRA_TESTDB_PORT", None, Some("3308".into())));
        m.insert("SPIRA_SNAP_STALE_S".into(), pick("SPIRA_SNAP_STALE_S", s.snap_stale_s.as_ref(), Some("60".into())));
        m.insert("DOLT".into(), self.env("DOLT").or_else(|| self.which("dolt")).unwrap_or_default());
        m
    }

    fn which(&self, prog: &str) -> Option<String> {
        let path = self.env("PATH")?;
        path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(prog)).find(|p| crate::fsutil::is_executable(p)).map(|p| p.display().to_string())
    }
}
