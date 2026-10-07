//! Small host readers: UTC timestamps, /proc/meminfo, /proc/stat, memory PSI, git.

use std::path::Path;
use std::process::{Command, Stdio};

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix epoch (proleptic Gregorian, UTC).
pub fn iso_utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let rem = epoch % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// A `/proc/meminfo` field in kB.
pub fn meminfo_kb(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let rest = l.strip_prefix(key)?.strip_prefix(':')?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// `(total, idle)` jiffies from the aggregate `cpu ` line of `/proc/stat` (idle = idle + iowait).
pub fn cpu_jiffies(text: &str) -> Option<(u64, u64)> {
    let l = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = l
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if v.len() < 5 {
        return None;
    }
    Some((v.iter().sum(), v[3] + v[4]))
}

pub fn cpu_busy_pct(a: (u64, u64), b: (u64, u64)) -> Option<u64> {
    let dt = b.0.checked_sub(a.0)?;
    let di = b.1.checked_sub(a.1)?;
    (dt > 0).then(|| 100 * (dt.saturating_sub(di)) / dt)
}

/// `some avg10=` from `/proc/pressure/memory`.
pub fn psi_some_avg10(text: &str) -> Option<f64> {
    let l = text.lines().find(|l| l.starts_with("some"))?;
    l.split_whitespace()
        .find_map(|f| f.strip_prefix("avg10=")?.parse().ok())
}

pub fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// `command -v <name>` over `path` (a PATH value): the first executable `<dir>/<name>`.
/// Harness scripts and Spira tools are found this way — on the launcher's PATH (sp-gypjk) —
/// never joined under the harness directory.
pub fn which_in(path: &str, name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

pub fn nproc() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

/// `git -C <dir> <args>`: trimmed stdout on success. Never inherits a caller's GIT_DIR.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    // batch-job: part of a testenv trial, which is bounded by the trial deadline, not per call
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown-host".into())
}

/// Close every fd ≥ 3 in the child before it execs. This process's OWN fds are already
/// close-on-exec (Rust's std opens them that way), but a caller's lock fd this process
/// never opened itself — inherited from a bash caller's `exec 9>…` with no O_CLOEXEC, or
/// from any process up the chain that spawned this one — carries none of that protection,
/// and `podman run`'s conmon (or a cargo build's auto-started sccache server) daemonizes
/// and would hold it forever (sp-ohwg7). `close_range` is the fast path; a kernel too old
/// for it (< 5.9) falls back to `fcntl(F_SETFD)` per fd. Only async-signal-safe calls run
/// between fork and exec.
pub fn close_inherited_fds(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            let r = libc::syscall(libc::SYS_close_range, 3u32, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC);
            if r != 0 {
                for fd in 3..4096 {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_matches_date() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        // date -u -d @1790000000 +%Y-%m-%dT%H:%M:%SZ
        assert_eq!(iso_utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn proc_readers() {
        let mi = "MemTotal:       65000000 kB\nMemAvailable:   32000000 kB\n";
        assert_eq!(meminfo_kb(mi, "MemAvailable"), Some(32_000_000));
        assert_eq!(meminfo_kb(mi, "Mem"), None);
        let a = cpu_jiffies("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 1 1 1 1\n").unwrap();
        assert_eq!(a, (1000, 800));
        assert_eq!(cpu_busy_pct(a, (2000, 1300)), Some(50));
        assert_eq!(cpu_busy_pct(a, a), None);
        assert_eq!(
            psi_some_avg10("some avg10=12.50 avg60=1.00 avg300=0.00 total=1\nfull avg10=0.00\n"),
            Some(12.5)
        );
    }
}
