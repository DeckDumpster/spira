//! The production [`crate::ports::World`].

use crate::ports::World;
use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Real {
    pub home: PathBuf,
    pub run: PathBuf,
    pub db: String,
    pub bd: String,
    pub repo_map: Option<PathBuf>,
    registry: OnceCell<spira_config::repos::Registry>,
    strand_cfg: OnceCell<strand::config::Config>,
}

impl Real {
    pub fn new(home: PathBuf, run: PathBuf, db: String, bd: String, repo_map: Option<PathBuf>) -> Real {
        Real { home, run, db, bd, repo_map, registry: OnceCell::new(), strand_cfg: OnceCell::new() }
    }

    /// `strand::config::Config`, resolved once per process the same way `strand`'s own
    /// binary resolves it (env, then spira.toml, then the conf.sh default) — the detectors
    /// moved there (wave 4.29, sp-8ofmt) and are reached in-process instead of through the
    /// `lib.sh` seam [`Real::seam`] still carries for `spira_open_trigger_count`/
    /// `spira_lane_admitted`.
    fn strand_cfg(&self) -> &strand::config::Config {
        self.strand_cfg.get_or_init(|| strand::config::Config::resolve(&strand::config::Live::load()))
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"): `spira_home_repo`/`repo_root`/`spira_landref` were already a bash
    /// subprocess sourcing lib.sh THEN shelling to the `spira-config` binary a second time
    /// (lib.sh's own shim, sp-37rmg) — two processes per lookup. `from_env` resolves
    /// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` in-process
    /// instead, the same way conf.sh does, when this (bare, unit-launched) process's own
    /// environment lacks them (sp-z3eyk) — reading the bare three and the map text by hand,
    /// as this used to, found nothing in production, since conf.sh exports none of them.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home))
    }

    fn repo_map_text_inner(&self) -> String {
        match &self.repo_map {
            Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// The `lib.sh` seam (same pattern as `gate-check/src/real.rs`): source `lib.sh`, then
    /// run `body` with `args` as positional parameters, capturing trimmed stdout.
    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(". \"$0\" >/dev/null 2>&1 || exit 96\n{body}");
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .args(args)
            .env("SPIRA_HOME", &self.home)
            .env("SPIRA_DB", &self.db)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        out.ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
            .unwrap_or_default()
    }

    fn seam_ok(&self, body: &str, args: &[&str]) -> bool {
        let script = format!(". \"$0\" >/dev/null 2>&1 || exit 96\n{body}");
        Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .args(args)
            .env("SPIRA_HOME", &self.home)
            .env("SPIRA_DB", &self.db)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn read_epoch_file(&self, name: &str) -> i64 {
        std::fs::read_to_string(self.run.join(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    }
}

impl World for Real {
    fn log(&self, msg: &str) {
        eprintln!(
            "{} maechen-trigger: {}",
            humantime_utc_now(),
            msg
        );
    }

    fn read_watermark(&self) -> i64 {
        self.read_epoch_file("maechen.watermark")
    }

    fn read_lastpass(&self) -> i64 {
        self.read_epoch_file("maechen.lastpass")
    }

    fn now(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    fn open_trigger_count(&self, labels: &str) -> u64 {
        self.seam("spira_open_trigger_count \"$1\"", &[labels]).parse().unwrap_or(0)
    }

    fn lane_admitted(&self, lane: &str) -> bool {
        self.seam_ok("spira_lane_admitted \"$1\"", &[lane])
    }

    fn home_repo(&self) -> String {
        self.registry().home_repo().to_string()
    }

    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.registry().root(name).map(PathBuf::from)
    }

    fn landref(&self, repo_path_or_name: &str) -> Option<String> {
        spira_config::repos::landref(self.registry(), repo_path_or_name)
    }

    fn repo_map_text(&self) -> String {
        self.repo_map_text_inner()
    }

    fn git_log_subjects(&self, repo_path: &Path, since_ts: i64, base_ref: &str) -> String {
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("log")
            .arg("--format=%s")
            .arg(format!("--after=@{since_ts}"))
            .arg(base_ref)
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn detect_invalid_closed(&self) -> String {
        strand::detectors::detect_invalid_closed(self.strand_cfg())
    }

    fn create_bead(&self, title: &str, labels: &str, description: &str) -> Result<(), String> {
        let mut child = Command::new(&self.bd)
            .arg("-C")
            .arg(&self.db)
            .arg("create")
            .arg(title)
            .arg("--type")
            .arg("task")
            .arg("--label")
            .arg(labels)
            .arg("--priority")
            .arg("3")
            .arg("--description")
            .arg(description)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot spawn bd create: {e}"))?;
        let stderr = child.stderr.take();
        let status = child.wait().map_err(|e| format!("bd create: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            let mut msg = String::new();
            if let Some(mut s) = stderr {
                use std::io::Read;
                let _ = s.read_to_string(&mut msg);
            }
            Err(format!("bd create exited {status}: {msg}"))
        }
    }
}

fn humantime_utc_now() -> String {
    // Matches the bash's `date -u +%Y-%m-%dT%H:%M:%SZ`. No chrono dependency: shell to
    // `date`, exactly as every other already-rewritten crate's log stamp does when it needs
    // one (czar-pass, reconciler) — a fixed-format UTC stamp is not worth a crate.
    Command::new("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%SZ")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
