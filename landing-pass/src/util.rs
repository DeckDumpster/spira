//! Small shared pieces: the UTC timestamp lib.sh's `log` prints, atomic writes, the text
//! helpers the notes are built from, and the one way this binary starts a child process.

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an epoch, without a subprocess.
pub fn iso_utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let rem = epoch % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Civil-to-days (Howard Hinnant's algorithm), `iso_utc`'s inverse half.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `datetime.fromisoformat(ts.replace("Z", "+00:00"))` as an epoch: `bead_context`'s age
/// needs this for `created_at`. Accepts an optional fractional second and a `Z` or
/// `+HH:MM`/`-HH:MM` offset; anything else (no offset, wrong punctuation) is None —
/// python's naive-datetime refusal, not a guess.
pub fn parse_iso(ts: &str) -> Option<i64> {
    let ts = ts.trim();
    if ts.len() < 19 {
        return None;
    }
    let b = ts.as_bytes();
    let num = |a: usize, z: usize| -> Option<i64> { ts.get(a..z)?.parse().ok() };
    if b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &ts[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        rest = &r[n..];
    }
    let off = if rest == "Z" {
        0
    } else if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && &rest[3..4] == ":" {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s - off)
}

/// Write `content` to `path` through a temp file and a rename, so no reader ever sees half a
/// record.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_file_name(format!(
        "{}.{}",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

pub fn append_line(path: &Path, line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// `printf '%s' "$s" | tail -N`, as a `$(…)` capture sees it (trailing newlines stripped).
pub fn tail_lines(s: &str, n: usize) -> String {
    let body = s.strip_suffix('\n').unwrap_or(s);
    if body.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = body.split('\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n").trim_end_matches('\n').to_string()
}

/// `printf '%s' "$s" | head -1`.
pub fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or("")
}

/// `printf '%s\n' "$s" | tail -c N`, cut back to a character boundary.
pub fn tail_bytes(s: &str, n: usize) -> String {
    let full = format!("{s}\n");
    if full.len() <= n {
        return full;
    }
    let mut start = full.len() - n;
    while !full.is_char_boundary(start) {
        start += 1;
    }
    full[start..].to_string()
}

/// `printf '%s' "$br" | tr -c 'A-Za-z0-9._-' '-'` — the noverdict counter key.
pub fn branch_key(br: &str) -> String {
    br.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect()
}

/// The pid of the child this process is waiting on, so a SIGTERM can be forwarded to it.
pub static CURRENT_CHILD: AtomicI32 = AtomicI32::new(0);

/// The pids of the gates the concurrent certification walk is waiting on (DESIGN.md §8
/// D14 (i)). Each one leads its own process group; a SIGTERM to the pass is forwarded to
/// every group in the set, not only to [`CURRENT_CHILD`].
pub struct ChildSet(std::sync::Mutex<std::collections::BTreeSet<i32>>);

impl ChildSet {
    pub const fn new() -> ChildSet {
        ChildSet(std::sync::Mutex::new(std::collections::BTreeSet::new()))
    }
    pub fn insert(&self, pid: i32) {
        if let Ok(mut s) = self.0.lock() {
            s.insert(pid);
        }
    }
    pub fn remove(&self, pid: i32) {
        if let Ok(mut s) = self.0.lock() {
            s.remove(&pid);
        }
    }
    pub fn pids(&self) -> Vec<i32> {
        self.0.lock().map(|s| s.iter().copied().collect()).unwrap_or_default()
    }
    /// TERM every registered process group; returns the pids signalled.
    pub fn term_all(&self) -> Vec<i32> {
        let pids = self.pids();
        for &p in &pids {
            if p > 0 {
                unsafe {
                    libc::kill(-p, libc::SIGTERM);
                }
            }
        }
        pids
    }
}

impl Default for ChildSet {
    fn default() -> Self {
        ChildSet::new()
    }
}

/// Every gate the concurrent walk has running (production's registry).
pub static GATE_CHILDREN: ChildSet = ChildSet::new();

/// Every child starts through here: with the default signal mask restored (this process
/// blocks TERM/INT in all threads so one thread can `sigwait` for them, and a child would
/// otherwise inherit the block and survive `halt`), and in a process group of its own, so a
/// forwarded TERM reaches the whole of it (gate.sh's own children too).
/// Whether `program` can be run: a path (it has a `/`) must exist; a bare name must be an
/// executable file in one of `PATH`'s directories — the launcher's PATH (sp-gypjk).
pub fn runnable(program: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let exec = |p: &Path| std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if program.as_os_str().is_empty() {
        return false;
    }
    if program.to_string_lossy().contains('/') {
        return std::fs::metadata(program).is_ok();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|d| exec(&d.join(program))))
        .unwrap_or(false)
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.process_group(0);
    unsafe {
        c.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            // Close every fd >= 3 before exec. This process's own fds are already
            // close-on-exec (Rust's std opens them that way), but a caller's lock fd this
            // process never opened itself — e.g. a bash caller's `exec 9>…` with no
            // O_CLOEXEC, surviving the fork+exec that started this binary — carries none of
            // that protection. Every child this function starts (gate.sh above all, which
            // runs testenv, which runs podman — conmon daemonizes and outlives the trial)
            // must not inherit it (sp-ohwg7). close_range is the fast path; a kernel too
            // old for it (< 5.9) falls back to fcntl(F_SETFD) per fd.
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
    c
}

/// Run a prepared command to completion, recording its pid for signal forwarding; returns
/// (exit code, stdout, stderr). A command that cannot start is (-1, "", error text).
pub fn run_capture(mut c: Command) -> (i32, Vec<u8>, Vec<u8>) {
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if c.get_program().is_empty() {
        return (-1, Vec::new(), b"empty program".to_vec());
    }
    let child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (-1, Vec::new(), e.to_string().into_bytes()),
    };
    CURRENT_CHILD.store(child.id() as i32, Ordering::SeqCst);
    let out = child.wait_with_output();
    CURRENT_CHILD.store(0, Ordering::SeqCst);
    match out {
        Ok(o) => (o.status.code().unwrap_or(-1), o.stdout, o.stderr),
        Err(e) => (-1, Vec::new(), e.to_string().into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_matches_date() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn iso_round_trips_and_refuses_naive() {
        assert_eq!(parse_iso("2026-09-29T03:47:18Z"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T03:47:18.123Z"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T05:47:18+02:00"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T03:47:18"), None);
        assert_eq!(parse_iso("garbage"), None);
        assert_eq!(parse_iso(&iso_utc(1_790_000_000)), Some(1_790_000_000));
    }

    #[test]
    fn tails_match_the_shell() {
        assert_eq!(tail_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(tail_lines("a\nb\nc", 5), "a\nb\nc");
        assert_eq!(tail_lines("", 3), "");
        assert_eq!(tail_bytes("abcdef", 3), "ef\n");
        assert_eq!(tail_bytes("ab", 10), "ab\n");
        assert_eq!(branch_key("spira/sp-a.1"), "spira-sp-a.1");
        assert_eq!(first_line("x\ny"), "x");
    }
}

#[cfg(test)]
mod runnable_tests {
    use super::runnable;
    use std::path::Path;

    #[test]
    fn a_bare_name_is_looked_up_on_path_and_a_path_is_statted() {
        assert!(runnable(Path::new("sh")), "sh is on every PATH");
        assert!(!runnable(Path::new("no-such-spira-tool-sp-gypjk")));
        assert!(runnable(Path::new("/bin/sh")));
        assert!(!runnable(Path::new("/nonexistent/incident.sh")));
        assert!(!runnable(Path::new("")));
    }
}
