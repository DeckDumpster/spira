//! The outage alarm (G6). Deciding WHEN to alarm (once per outage) is the pool's job; this
//! only delivers one.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

pub trait Alarm {
    fn raise(&self, reason: &str);
}

/// Mails through the harness's own `mail.sh send` (a subprocess, never reimplemented),
/// invoked by name on the launcher's PATH (sp-gypjk); `mail` is a field only so a unit test
/// can hand in a recorder.
pub struct MailAlarm {
    pub mail: PathBuf,
    pub mailbox: String,
    pub retry_secs: u64,
}

pub fn alarm_body(reason: &str, retry_secs: u64) -> String {
    format!("## Alert\nround-vm: {reason}\n\nThe round has not started; retrying every {retry_secs}s.\n")
}

impl Alarm for MailAlarm {
    fn raise(&self, reason: &str) {
        eprintln!("round-vm: outage: {reason}");
        let mail = &self.mail;
        let child = crate::procs::command(mail)
            .arg("send")
            .arg(&self.mailbox)
            .arg("--from")
            .arg("Round VM <round-vm@spira>")
            .arg("--subject")
            .arg(format!("round VM outage: {reason}"))
            .arg("--kind")
            .arg("alert")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                if let Some(mut si) = c.stdin.take() {
                    let _ = si.write_all(alarm_body(reason, self.retry_secs).as_bytes());
                }
                let _ = c.wait();
            }
            Err(e) => eprintln!("round-vm: cannot run {}: {e}", mail.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn mails_through_mail_sh_send_with_the_reason() {
        let d = TempDir::new();
        let log = d.path().join("log");
        let mail = d.path().join("mail.sh");
        testkit::write_exe(&mail, &format!("#!/bin/sh\necho \"$@\" > {0}\ncat >> {0}\n", log.display()));
        MailAlarm { mail: mail.clone(), mailbox: "operator".into(), retry_secs: 60 }
            .raise("API unreachable (nextid)");
        let got = std::fs::read_to_string(&log).unwrap();
        assert!(got.starts_with("send operator --from Round VM <round-vm@spira> --subject round VM outage: API unreachable (nextid) --kind alert"), "{got}");
        assert!(got.contains("retrying every 60s"), "{got}");
    }
}
