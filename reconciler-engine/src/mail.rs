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
    let err_path = std::env::temp_dir().join(format!("mail-send-{}-{}.err", std::process::id(), mailbox));
    let err_file = std::fs::File::create(&err_path).map_err(|e| format!("{}: {e}", err_path.display()))?;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(err_file)
        .spawn()
        .map_err(|e| format!("{mail_bin}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let status = child.wait().map_err(|e| format!("{mail_bin}: {e}"));
    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
    let _ = std::fs::remove_file(&err_path);
    let status = status?;
    if !status.success() {
        return Err(format!("{mail_bin} send {mailbox}: exit {}: {}", status.code().unwrap_or(-1), stderr.trim()));
    }
    Ok(())
}

pub fn note_concierge(mail_bin: &str, subject: &str, body: &str) -> Result<(), String> {
    send(mail_bin, "concierge", FROM, subject, "note", None, None, body)
}
