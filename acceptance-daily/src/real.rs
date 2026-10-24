use crate::{Verdict, World};
use spira_config::bounded::{bounded, bounded_for};
use spira_otel::metrics::{MetricsSink, Point};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];
const RUN_SECS: &str = "10800";
pub const POLL_SECS: u64 = 60;

pub struct Real {
    pub repo: String,
    pub land_ref: String,
    pub run: PathBuf,
    pub tree: PathBuf,
    pub accept: String,
    pub bead: String,
    pub lc: String,
    pub sink: MetricsSink,
}

fn git(repo: &str, args: &[&str]) -> Result<String, String> {
    let o = bounded("git").arg("-C").arg(repo).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A gate-worker slot holds its flock for as long as it certifies a round.
pub fn slot_locked(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("worker.lock")).any(|e| {
        let Ok(f) = std::fs::OpenOptions::new().write(true).open(e.path()) else { return false };
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
    })
}

impl Real {
    fn bead_cmd(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
        let o = bounded_for(&self.bead, "120").envs(spira_config::release_env::child_path_env_for_process()).args(args).output().map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(format!("bead.sh exited {}: {}", o.status, String::from_utf8_lossy(&o.stderr).trim()))
        }
    }

    fn with_body<T>(&self, body: &str, f: impl FnOnce(&str) -> Result<T, String>) -> Result<T, String> {
        let p = std::env::temp_dir().join(format!("acceptance-daily-{}.md", std::process::id()));
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        let r = f(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        r
    }
}

impl World for Real {
    fn round_busy(&self) -> bool {
        slot_locked(&self.run.join("gate-worker"))
    }

    fn sleep(&self) {
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
    }

    fn day(&self) -> String {
        Command::new("date").arg("+%Y-%m-%d").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    }

    fn head(&self) -> Result<String, String> {
        git(&self.repo, &["rev-parse", "--verify", "-q", &format!("{}^{{commit}}", self.land_ref)])
    }

    fn prepare_tree(&self, commit: &str) -> Result<(), String> {
        self.release_tree();
        let tree = self.tree.to_string_lossy();
        git(&self.repo, &["worktree", "prune"])?;
        git(&self.repo, &["worktree", "add", "-q", "--detach", &tree, commit]).map(|_| ())
    }

    fn release_tree(&self) {
        let tree = self.tree.to_string_lossy();
        let _ = git(&self.repo, &["worktree", "remove", "--force", &tree]);
        let _ = std::fs::remove_dir_all(&self.tree);
    }

    fn forensics(&self, commit: &str) -> String {
        self.run.join("acceptance-daily").join(format!("forensics-{}", &commit[..commit.len().min(12)])).to_string_lossy().into_owned()
    }

    fn run(&self, forensics: &str) -> (i32, String) {
        // batch-job: a fresh-install acceptance builds the release and boots a container
        let o = Command::new("timeout")
            .args([RUN_SECS, &self.accept])
            .arg(&self.tree)
            .env("SPIRA_ACCEPTANCE_LOCAL_FORENSICS", forensics)
            .output();
        match o {
            Ok(o) => {
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                (o.status.code().unwrap_or(-1), text)
            }
            Err(e) => (-1, format!("cannot start {}: {e}", self.accept)),
        }
    }

    fn is_open(&self, id: &str) -> Result<bool, String> {
        let o = bounded(&self.lc).args(["show", id]).output().map_err(|e| e.to_string())?;
        match o.status.code() {
            Some(1) => Ok(false),
            Some(0) => {
                let doc: serde_json::Value = serde_json::from_slice(&o.stdout).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok(!TERMINAL.contains(&state))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", String::from_utf8_lossy(&o.stdout).trim())),
        }
    }

    fn file(&self, title: &str, body: &str) -> Result<String, String> {
        let out = self.with_body(body, |p| self.bead_cmd(&["file", title, "--for", "builder", "--repo", "spira", "--priority", "1", "--json", "--body-file", p]))?;
        let start = out.find(['{', '[']).ok_or("bead.sh file: no JSON in output")?;
        let v: serde_json::Value = serde_json::from_str(&out[start..]).map_err(|e| format!("bead.sh file: {e}"))?;
        let v = if let serde_json::Value::Array(a) = v { a.into_iter().next().unwrap_or_default() } else { v };
        v.get("id").and_then(|i| i.as_str()).map(String::from).ok_or_else(|| "bead.sh file: no id in output".into())
    }

    fn append(&self, id: &str, body: &str, note: &str) -> Result<(), String> {
        self.with_body(body, |p| self.bead_cmd(&["amend", id, "--append-body", p, "--note", note])).map(|_| ())
    }

    fn record(&self, commit: &str, v: &Verdict) {
        let value = match v {
            Verdict::Pass => 1.0,
            Verdict::Fail(_) => 0.0,
            Verdict::NotRun(_) => -1.0,
        };
        self.sink.push(&[Point::Gauge { name: "spira_acceptance_verdict", value }], &[("commit", &commit[..commit.len().min(12)])]);
    }
}
