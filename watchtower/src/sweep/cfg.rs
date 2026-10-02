//! Every threshold and path the sweep reads, with the bash's own defaults. `main.rs`
//! builds this from the environment once; everything downstream takes it by reference.

use std::path::{Path, PathBuf};

pub struct Cfg {
    pub spira_run: PathBuf,
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
    pub gate_log: Option<PathBuf>,
    pub yield_window_s: i64,
    pub yield_sh: Option<String>,
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
    pub systemctl: String,
    pub journalctl: String,

    pub prompt_file: Option<PathBuf>,
    pub lapsed_dir: Option<PathBuf>,
    pub lapsed_marker: Option<PathBuf>,
    pub failed_units_state: Option<PathBuf>,
    pub throttle_stamp: Option<PathBuf>,
    pub queue_throttle_override: String,

    pub incident_sh: String,
    pub bd: String,
    pub suites_sh: Option<String>,
    pub moot_sh: Option<String>,
    pub branch_guard_sh: Option<String>,
}

impl Cfg {
    pub fn landstate_dir(&self) -> PathBuf {
        self.spira_run.join("landstate")
    }
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
    pub fn moot_sh(&self) -> String {
        self.moot_sh.clone().unwrap_or_else(|| {
            Path::new(&self.lib_sh_dir)
                .parent()
                .map(|p| p.join("cockpit/moot-sweep.sh").to_string_lossy().into_owned())
                .unwrap_or_else(|| "cockpit/moot-sweep.sh".to_string())
        })
    }
}
