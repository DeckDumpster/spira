//! The production [`crate::ports::World`]: git, `/proc`, the filesystem, `setsid`.

use crate::ports::World;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Real {
    pub home: PathBuf,
}

impl Real {
    pub fn new(home: PathBuf) -> Real {
        Real { home }
    }

    fn exe(&self) -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("gate-run"))
    }
}

const REPO_CONTEXT: &str = r#"set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 96
RN="$2"
if [ -r "${SPIRA_REPO_MAP:-/nonexistent}" ] && r="$(repo_root "$RN")"; then
    printf 'repo_root=%s\n' "$r"
fi
"#;

const LANDREF_SNIPPET: &str = r#"set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 96
spira_landref "$2"
"#;

impl World for Real {
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String> {
        let o = Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("rev-parse")
            .arg("--verify")
            .arg("-q")
            .arg(format!("{rev}^{{commit}}"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
        } else {
            None
        }
    }

    fn resolve_repo(&self, repo_name: &str) -> Result<PathBuf, String> {
        let o = Command::new("bash")
            .arg("-c")
            .arg(REPO_CONTEXT)
            .arg("gate-run-context")
            .arg(&self.home)
            .arg(repo_name)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("bash: {e}"))?;
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("repo_root=") {
                if !v.is_empty() {
                    return Ok(PathBuf::from(v));
                }
            }
        }
        Err(format!("gate-run: the repository map has no entry for '{repo_name}' — refusing to guess a checkout"))
    }

    fn landref(&self, repo: &Path) -> Option<String> {
        let o = Command::new("bash")
            .arg("-c")
            .arg(LANDREF_SNIPPET)
            .arg("gate-run-landref")
            .arg(&self.home)
            .arg(repo)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string();
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        }
    }

    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }

    fn read(&self, p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim_end_matches('\n').to_string())
    }

    fn read_raw(&self, p: &Path) -> Option<String> {
        fs::read_to_string(p).ok()
    }

    fn read_i32(&self, p: &Path) -> Option<i32> {
        self.read(p).and_then(|s| s.trim().parse::<i32>().ok())
    }

    fn mkdir_p(&self, p: &Path) {
        let _ = fs::create_dir_all(p);
    }

    fn write_atomic(&self, p: &Path, content: &str) {
        let dir = p.parent().unwrap_or_else(|| Path::new("."));
        let tmp = dir.join(format!(".{}.{}", p.file_name().and_then(|n| n.to_str()).unwrap_or("tmp"), std::process::id()));
        if fs::write(&tmp, content).is_ok() {
            let _ = fs::rename(&tmp, p);
        }
    }

    fn remove_dir_all(&self, p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }

    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    fn pid(&self) -> i64 {
        std::process::id() as i64
    }

    fn proc_exists(&self, pid: i64) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    fn proc_cmdline(&self, pid: i64) -> Option<String> {
        let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(
            raw.split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn proc_pgid(&self, pid: i64) -> Option<i64> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 2 (comm) can contain spaces/parens, so split after its closing ')'.
        let after = stat.rsplit_once(')')?.1;
        after.split_whitespace().nth(2)?.parse().ok()
    }

    fn list_other_procs(&self, exclude: i64) -> Vec<(i64, String)> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else { return out };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid_str) = name.to_str() else { continue };
            let Ok(pid) = pid_str.parse::<i64>() else { continue };
            if pid == exclude {
                continue;
            }
            if let Some(cmd) = self.proc_cmdline(pid) {
                if !cmd.is_empty() {
                    out.push((pid, cmd));
                }
            }
        }
        out
    }

    fn kill(&self, pid: i64, group: bool) {
        unsafe {
            if group {
                libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
            }
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    fn spawn_detached(&self, branch: &str, repo_name: &str) {
        let exe = self.exe();
        let use_setsid = Command::new("sh").arg("-c").arg("command -v setsid").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        let mut c = if use_setsid {
            let mut c = Command::new("setsid");
            c.arg(&exe);
            c
        } else {
            Command::new(&exe)
        };
        c.arg("--home").arg(&self.home).arg("--exec").arg(branch).arg(repo_name);
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // Detach fully: do not hold a handle that would keep this process waiting on the
        // child, and do not let the child's own exit become this process's problem.
        let _ = c.spawn();
    }

    fn run_gate(&self, _home: &Path, branch: &str, repo_name: &str, out_path: &Path) -> i32 {
        let out_file = match fs::OpenOptions::new().create(true).write(true).truncate(true).open(out_path) {
            Ok(f) => f,
            Err(_) => return 1,
        };
        let err_file = match out_file.try_clone() {
            Ok(f) => f,
            Err(_) => return 1,
        };
        // gate.sh by name on the launcher's PATH (sp-gypjk).
        let status = Command::new("gate.sh")
            .arg(branch)
            .arg(repo_name)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_file))
            .stderr(Stdio::from(err_file))
            .status();
        status.ok().and_then(|s| s.code()).unwrap_or(1)
    }

    fn eprintln(&self, s: &str) {
        let _ = writeln!(std::io::stderr(), "{s}");
    }

    fn print(&self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}
