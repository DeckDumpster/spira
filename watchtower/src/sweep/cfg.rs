//! Every threshold and path the sweep reads, with the bash's own defaults. `main.rs`
//! builds this from the environment once; everything downstream takes it by reference.

use std::path::PathBuf;

pub struct Cfg {
    pub spira_run: PathBuf,
    /// Where the scheduler keeps the cockpit document; none without a runtime directory.
    pub cockpit_view_dir: Option<PathBuf>,
    /// The REAL `spira/` install directory (where `lib.sh` and `cockpit/moot-sweep.sh`'s
    /// sibling actually live), found via `main::lib_sh_dir` — never `$SPIRA_HOME`, which a
    /// caller may point elsewhere for data purposes while still expecting lib.sh's actual
    /// code (sp-lnmbq: the idle-while-ready fixture does exactly this).
    pub lib_sh_dir: String,
    pub db: String,
    pub home_repo: String,
    pub ask_label: String,

    pub snap_stale_s: i64,
    pub gate_window_s: i64,
    pub gate_silence_window_s: i64,
    pub gate_deadline_s: i64,
    pub gate_log: Option<PathBuf>,
    pub noverdict_window_s: i64,
    pub noverdict_max: i64,
    pub yield_window_s: i64,
    pub yield_dir: PathBuf,
    pub disk_warn_pct: i64,
    pub mem_warn_mb: i64,
    pub meminfo_path: PathBuf,
    pub failed_units_warn_mins: i64,
    pub cpu_throttle_units: Vec<String>,
    pub cpu_throttle_warn_pct: i64,
    pub cpu_throttle_min_periods: i64,
    pub cgroup_root: String,
    pub unsent_warn_h: i64,
    pub closed_stranded_warn_h: i64,
    pub drain_warn_mins: i64,
    pub idle_while_ready_n: usize,
    pub units: std::sync::Arc<dyn runtime::units::Units>,

    pub prompt_file: Option<PathBuf>,
    pub lapsed_dir: Option<PathBuf>,
    pub lapsed_marker: Option<PathBuf>,
    pub failed_units_state: Option<PathBuf>,
    pub throttle_stamp: Option<PathBuf>,
    pub queue_throttle_override: String,

    pub incident_sh: String,
    /// The store's client: bd's rows through its content door.
    pub lc: spira_store::lc_call::LcClient,
    /// `COCKPIT_DB`; `None` when it does not resolve (the moot-sweep then checks nothing).
    pub cockpit_db: Option<String>,
    /// `SPIRA_MOOT_TIMEOUT`.
    pub moot_timeout_s: u64,
    /// `SPIRA_RESOLVE_SH`, else `resolve`.
    pub resolve_bin: String,
    /// `spira-lc`: whether an incident bead is unfinished is its lifecycle row's answer.
    pub lc_bin: String,
}

impl Cfg {
    pub fn prompt_file(&self) -> PathBuf {
        self.prompt_file
            .clone()
            .unwrap_or_else(|| self.spira_run.join("ops-sweep-prompt.txt"))
    }
    pub fn lapsed_dir(&self) -> PathBuf {
        self.lapsed_dir.clone().unwrap_or_else(|| self.spira_run.join("lapsed"))
    }
    pub fn lapsed_marker(&self) -> PathBuf {
        self.lapsed_marker
            .clone()
            .unwrap_or_else(|| self.spira_run.join("lapsed.swept"))
    }
    pub fn failed_units_state(&self) -> PathBuf {
        self.failed_units_state
            .clone()
            .unwrap_or_else(|| self.spira_run.join("failed-units.state"))
    }
    pub fn throttle_stamp(&self) -> PathBuf {
        self.throttle_stamp
            .clone()
            .unwrap_or_else(|| self.spira_run.join("queue-throttled"))
    }
    pub fn gate_log(&self) -> PathBuf {
        self.gate_log.clone().unwrap_or_else(|| self.spira_run.join("gate.log"))
    }
    pub fn moot(&self) -> super::moot::MootCfg {
        super::moot::MootCfg {
            db: self.cockpit_db.clone(),
            lc: self.lc.clone(),
            ask_label: self.ask_label.clone(),
            timeout_s: self.moot_timeout_s,
            resolve: self.resolve_bin.clone(),
        }
    }
}

#[cfg(test)]
impl Cfg {
    /// A sweep config over `d` with every threshold at a plain value, every path inside `d`, and
    /// no live store, unit or binary behind it.
    pub fn fixture(d: &std::path::Path, incident_sh: String) -> Cfg {
        Cfg {
            spira_run: d.to_path_buf(),
            cockpit_view_dir: Some(d.join("view")),
            lib_sh_dir: String::new(),
            db: "db".into(),
            home_repo: "spira".into(),
            ask_label: "needs-operator".into(), // literal-ok: test fixture
            snap_stale_s: 60,
            gate_window_s: 21600,
            gate_silence_window_s: 3600,
            gate_deadline_s: 300,
            gate_log: None,
            noverdict_window_s: 172800,
            noverdict_max: 3,
            yield_window_s: 86400,
            yield_dir: d.join("gate-yield"),
            disk_warn_pct: 90,
            mem_warn_mb: 1500,
            meminfo_path: PathBuf::from(format!("{}/meminfo", system_paths::PROC)),
            failed_units_warn_mins: 15,
            cpu_throttle_units: Vec::new(),
            cpu_throttle_warn_pct: 5,
            cpu_throttle_min_periods: 100,
            cgroup_root: system_paths::SYS_FS_CGROUP.into(),
            unsent_warn_h: 24,
            closed_stranded_warn_h: 48,
            drain_warn_mins: 15,
            idle_while_ready_n: 5,
            units: std::sync::Arc::new(runtime::units::ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)))),
            prompt_file: None,
            lapsed_dir: None,
            lapsed_marker: None,
            failed_units_state: Some(d.join("failed-units.state")),
            throttle_stamp: None,
            queue_throttle_override: String::new(),
            incident_sh,
            lc: spira_store::lc_call::LcClient::socket("does/not/exist/lc.sock"),
            cockpit_db: None,
            moot_timeout_s: 30,
            resolve_bin: "resolve".into(),
            lc_bin: "does/not/exist/spira-lc".into(),
        }
    }
}
