//! `_repeat_check`/`_repeat_stamp`/`_repeat_release`: refuses a repeat mail to the same
//! recipient within `SPIRA_MAIL_REPEAT_WINDOW`. Fingerprint is (caller binary, normalised
//! subject, mailbox); normalisation strips digits and hex-looking runs so "requeued 5
//! times" and "requeued 6 times" collapse to the same escalation.
//!
//! ATOMICITY (sp-ifh5h): a per-fingerprint flock is held from [`repeat_check`] until
//! [`repeat_stamp`] (or [`repeat_release`] on an abandoned send), so a second caller for the
//! same fingerprint blocks rather than reading the stamp before the first caller writes it.
//!
//! BOUNDED WAIT (sp-y59a6): mail.sh's own `flock` here was a bare blocking call with no
//! timeout — a stale lock from a prior crashed/killed invocation hung every later send for
//! that fingerprint forever (observed: 30+ minutes). This port polls a non-blocking flock up
//! to `SPIRA_MAIL_LOCK_TIMEOUT_MS` (default 30s) and refuses loudly past it instead — fail
//! closed rather than hang, matching the rewrite programme's own rule (wave-brief, "every
//! check you port refuses when it cannot check"). No test in this workspace holds the lock
//! anywhere near 30s, so this is strictly additive: real contention (sub-second, G-11)
//! behaves exactly as before.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The direct parent's argv[0]/argv[1] basename — `caller` in mail.sh's `_repeat_check`.
/// Unreadable or gone (a short-lived intermediate shell, sp-9by2e's TOCTOU) reads as
/// "unknown" rather than failing the send.
pub fn caller_name() -> String {
    let ppid = unsafe { libc::getppid() };
    let path = format!("/proc/{ppid}/cmdline");
    let mut buf = Vec::new();
    if File::open(&path).and_then(|mut f| f.read_to_end(&mut buf)).is_err() {
        return "unknown".to_string();
    }
    let parts: Vec<&str> = buf.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| std::str::from_utf8(s).unwrap_or("")).collect();
    let c0 = parts.first().copied().unwrap_or("");
    let c1 = parts.get(1).copied().unwrap_or("");
    let src = if c1.is_empty() || c1.starts_with('-') { c0 } else { c1 };
    let src = if src.is_empty() { "unknown" } else { src };
    Path::new(src).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "unknown".to_string())
}

/// Strips digit runs and hex-looking runs of 7+ characters, then squeezes repeated spaces
/// into one — mail.sh's `sed -E 's/[0-9a-f]{7,}[0-9a-f]*//gI; s/[0-9]+//g' | tr -s ' '`.
pub fn normalize_subject(subject: &str) -> String {
    let hex_run = regex::Regex::new(r"(?i)[0-9a-f]{7,}").unwrap();
    let after_hex = hex_run.replace_all(subject, "");
    let digits = regex::Regex::new(r"[0-9]+").unwrap();
    let after_digits = digits.replace_all(&after_hex, "");
    let mut out = String::with_capacity(after_digits.len());
    let mut last_was_space = false;
    for ch in after_digits.chars() {
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(ch);
    }
    out
}

pub fn fingerprint(caller: &str, norm_subject: &str, mailbox: &str) -> String {
    let digest = Sha256::digest(format!("{caller}|{norm_subject}|{mailbox}").as_bytes());
    let mut hex = String::with_capacity(48);
    for b in digest.iter().take(24) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[derive(Debug)]
pub struct RepeatCheck {
    pub fp: String,
    stamp_dir: PathBuf,
    // Held only for its Drop effect: dropping the File closes the fd, which releases
    // the flock. Never read directly.
    #[allow(dead_code)]
    lock: Option<File>,
}

/// `run_dir/mail-repeat`.
fn stamp_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("mail-repeat")
}

/// Checks and, on success, leaves the fingerprint's lock held in the returned
/// [`RepeatCheck`] — the caller must follow with [`repeat_stamp`] on delivery or
/// [`repeat_release`] on any abandoned path (a lint failure, an error return), or the lock
/// leaks for the life of the process.
pub fn repeat_check(
    run_dir: &Path,
    mailbox: &str,
    subject: &str,
    window_s: u64,
    lock_timeout_ms: u64,
    considered: Option<&str>,
) -> Result<RepeatCheck, String> {
    // `SPIRA_MAIL_REPEAT_CONSIDERED` bypasses the guard entirely (mail.sh: checked before
    // anything else in `_repeat_check`) — no lock is taken, nothing is stamped.
    if considered.is_some() {
        return Ok(RepeatCheck { fp: String::new(), stamp_dir: stamp_dir(run_dir), lock: None });
    }
    let caller = caller_name();
    let norm = normalize_subject(subject);
    let fp = fingerprint(&caller, &norm, mailbox);

    let dir = stamp_dir(run_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mail: repeat guard: cannot create {}: {e}", dir.display()))?;
    let lock_path = dir.join(format!("{fp}.lock"));
    let lock_file = OpenOptions::new().create(true).write(true).open(&lock_path).map_err(|_| {
        format!("mail: repeat guard: cannot open lock for {mailbox}")
    })?;

    let deadline = Instant::now() + Duration::from_millis(lock_timeout_ms.max(1));
    loop {
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "mail: repeat guard: timed out after {lock_timeout_ms}ms waiting for {mailbox}'s per-fingerprint lock — refusing rather than hang (sp-y59a6); override: SPIRA_MAIL_REPEAT_CONSIDERED=<reason>"
            ));
        }
        sleep(Duration::from_millis(20));
    }

    let stamp_file = dir.join(&fp);
    if let Ok(meta) = std::fs::metadata(&stamp_file) {
        if let Ok(modified) = meta.modified() {
            let stamp_secs = modified.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let elapsed = now.saturating_sub(stamp_secs);
            if elapsed < window_s {
                let refused_file = dir.join(format!("{fp}.refused"));
                let mut f = OpenOptions::new().create(true).append(true).open(&refused_file)
                    .map_err(|e| format!("mail: repeat guard: cannot record refusal: {e}"))?;
                let _ = writeln!(f, "1");
                let nrefused = std::fs::read_to_string(&refused_file).map(|s| s.lines().count()).unwrap_or(1);
                // Release the lock before returning the refusal — no delivery follows.
                drop(lock_file);
                return Err(format!(
                    "mail: repeat refused — {caller} already sent to {mailbox} within {window_s}s window (refusals: {nrefused}) — override: SPIRA_MAIL_REPEAT_CONSIDERED=<reason>"
                ));
            }
        }
    }

    Ok(RepeatCheck { fp, stamp_dir: dir, lock: Some(lock_file) })
}

/// Stamps the fingerprint (delivery succeeded) and releases the lock. A no-op for the
/// `SPIRA_MAIL_REPEAT_CONSIDERED` bypass path (empty fingerprint, no lock held).
pub fn repeat_stamp(rc: RepeatCheck) {
    if rc.fp.is_empty() {
        return;
    }
    let stamp_file = rc.stamp_dir.join(&rc.fp);
    let _ = File::create(&stamp_file);
    // lock dropped (and closed) at end of scope, releasing flock.
}

/// Releases the lock without stamping — every abandoned path between a passing
/// [`repeat_check`] and delivery (e.g. a lint failure).
pub fn repeat_release(_rc: RepeatCheck) {
    // lock dropped (and closed) at end of scope, releasing flock.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_normalised_subject_produces_the_same_fingerprint() {
        let a = normalize_subject("Spira bead sp-abc — requeued 5 times, never landed — harness cannot land it");
        let b = normalize_subject("Spira bead sp-abc — requeued 6 times, never landed — harness cannot land it");
        assert_eq!(a, b);
    }

    #[test]
    fn different_subject_produces_a_different_fingerprint() {
        let a = normalize_subject("Spira bead sp-xyz — poisoned after 3 attempts — change the approach or drop it?");
        let b = normalize_subject("Spira bead sp-abc — requeued 5 times, never landed — harness cannot land it");
        assert_ne!(a, b);
    }

    #[test]
    fn a_second_check_for_the_same_fingerprint_within_the_window_is_refused_after_a_stamp() {
        let d = testkit::TempDir::new("mail-repeat");
        let rc1 = repeat_check(d.path(), "operator", "subject one", 3600, 5000, None).expect("first check passes");
        repeat_stamp(rc1);
        let err = repeat_check(d.path(), "operator", "subject one", 3600, 5000, None).unwrap_err();
        assert!(err.contains("already sent"), "{err}");
    }

    #[test]
    fn a_released_check_does_not_block_a_later_one() {
        let d = testkit::TempDir::new("mail-repeat");
        let rc1 = repeat_check(d.path(), "operator", "subject two", 3600, 5000, None).expect("first check passes");
        repeat_release(rc1);
        assert!(repeat_check(d.path(), "operator", "subject two", 3600, 5000, None).is_ok());
    }

    #[test]
    fn a_held_lock_times_out_rather_than_hanging_forever() {
        let d = testkit::TempDir::new("mail-repeat");
        let rc1 = repeat_check(d.path(), "operator", "subject three", 3600, 5000, None).expect("first check passes");
        let start = Instant::now();
        let err = repeat_check(d.path(), "operator", "subject three", 3600, 150, None).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2), "took {:?}", start.elapsed());
        assert!(err.contains("timed out"), "{err}");
        repeat_release(rc1);
    }
}

#[cfg(test)]
mod considered_override {
    use super::*;

    #[test]
    fn considered_override_bypasses_the_guard_and_stamps_nothing() {
        let d = testkit::TempDir::new("mail-repeat-override");
        let rc1 = repeat_check(d.path(), "operator", "subject", 3600, 5000, Some("testing")).unwrap();
        repeat_stamp(rc1);
        // No stamp was written, so an unrelated real check for the same fingerprint still passes.
        assert!(repeat_check(d.path(), "operator", "subject", 3600, 5000, None).is_ok());
    }
}
