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

/// Every child starts through here: with the default signal mask restored (this process
/// blocks TERM/INT in all threads so one thread can `sigwait` for them, and a child would
/// otherwise inherit the block and survive `halt`), and in a process group of its own, so a
/// forwarded TERM reaches the whole of it (gate.sh's own children too).
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.process_group(0);
    unsafe {
        c.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
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
