//! The alert path's IO seam: persists which streak each invariant last alerted for (one
//! JSON file, keyed by invariant), asks whether the Concierge is running, and hands mail to
//! `mail.sh`. Every function here does exactly one read, one write or one subprocess call —
//! the dedup and classification logic lives in `reconciler_engine::alert`, not here.

use std::io::Write;
use std::process::{Command, Stdio};

// Persistence for which streak each invariant last alerted for lives in reconciler-engine::io
// now — reconciler-flow shares the exact same file format instead of a second copy of it.
pub use reconciler_engine::io::{load_alerted, save_alerted, AlertedSinceMap};

/// True if `concierge.sh status` reports a live session — false for "not running" AND for
/// "cannot tell" (concierge.sh missing, or the call itself failed): either way there is
/// nobody to wake, so the alert must fall back to operator mail rather than being typed
/// into a session that will never read it.
pub fn concierge_is_running(concierge_sh: &str) -> bool {
    Command::new("bash")
        .arg(concierge_sh)
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Sends one message through `mail.sh send`. A spawn failure, a non-zero exit, or invalid
/// UTF-8 in stderr are all reported distinctly (mirroring czar-pass's `run_forge`) so a
/// caller never mistakes "mail.sh refused it" for "the wake was delivered".
pub fn mail_send(
    mail_sh: &str,
    mailbox: &str,
    from: &str,
    subject: &str,
    kind: &str,
    default: Option<&str>,
    body: &str,
) -> Result<(), String> {
    let mut cmd = Command::new("bash");
    cmd.arg(mail_sh).arg("send").arg(mailbox).arg("--from").arg(from).arg("--subject").arg(subject).arg("--kind").arg(kind);
    if let Some(d) = default {
        cmd.arg("--default").arg(d);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("mail.sh spawn failed: {}", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("mail.sh: failed to wait: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("mail.sh send {} exited {}: {}", mailbox, output.status, stderr.trim()));
    }
    Ok(())
}
