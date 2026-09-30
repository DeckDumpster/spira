//! The production [`crate::ports::World`].

use crate::ports::World;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Real {
    pub home: PathBuf,
    pub run: PathBuf,
    pub db: String,
    pub bd: String,
    pub repo_map: Option<PathBuf>,
}

impl Real {
    pub fn new(home: PathBuf, run: PathBuf, db: String, bd: String, repo_map: Option<PathBuf>) -> Real {
        Real { home, run, db, bd, repo_map }
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
        self.seam("spira_home_repo", &[])
    }

    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        let out = self.seam("repo_root \"$1\" 2>/dev/null", &[name]);
        if out.is_empty() {
            None
        } else {
            Some(PathBuf::from(out))
        }
    }

    fn landref(&self, repo_path_or_name: &str) -> Option<String> {
        let out = self.seam("spira_landref \"$1\" 2>/dev/null", &[repo_path_or_name]);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn repo_map_text(&self) -> String {
        match &self.repo_map {
            Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
            None => String::new(),
        }
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
        self.seam("detect_invalid_closed 2>/dev/null", &[])
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
