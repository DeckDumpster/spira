//! The one door a reconciler pass uses to wake the Concierge: `mail send`, by whatever
//! program the caller's seams name.

use std::io::Write;
use std::process::Stdio;

pub const FROM: &str = "Reconciler <reconciler@spira>";

/// One message through `mail send`. A spawn failure and a non-zero exit are reported
/// distinctly, so a caller never mistakes "mail refused it" for "the wake was delivered".
#[allow(clippy::too_many_arguments)]
pub fn send(
    mail_bin: &str,
    mailbox: &str,
    from: &str,
    subject: &str,
    kind: &str,
    default: Option<&str>,
    class: Option<&str>,
    body: &str,
) -> Result<(), String> {
    let mut cmd = spira_config::bounded::bounded(mail_bin);
    cmd.arg("send").arg(mailbox).arg("--from").arg(from).arg("--subject").arg(subject).arg("--kind").arg(kind);
    if let Some(d) = default {
        cmd.arg("--default").arg(d);
    }
    if let Some(c) = class {
        cmd.arg("--class").arg(c);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{mail_bin}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("{mail_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{mail_bin} send {mailbox}: exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

pub fn note_concierge(mail_bin: &str, subject: &str, body: &str) -> Result<(), String> {
    send(mail_bin, "concierge", FROM, subject, "note", None, None, body)
}
