//! The one door a reconciler pass uses to wake the Concierge: the mail library, in-process,
//! under the same environment and bead store `mail send` loads — no `mail` child.

use mail::bead::{Bd, BdCli};
use mail::cmds::SendArgs;
use mail::env::Env;

pub const FROM: &str = "Reconciler <reconciler@spira>";

/// One message, as `mail send` took it on its command line and stdin.
#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    pub mailbox: &'a str,
    pub from: &'a str,
    pub subject: &'a str,
    pub kind: &'a str,
    pub default: Option<&'a str>,
    pub class: Option<&'a str>,
    pub bead: Option<&'a str>,
    pub body: &'a str,
}

/// One message under this process's configured mail environment and bead store. A refusal
/// is reported naming the mailbox, so a caller never mistakes "mail refused it" for "the
/// wake was delivered".
pub fn send(m: &Message) -> Result<(), String> {
    let env = Env::load()?;
    // The ask's bead is filed through the store's content door (mail's BdCli).
    let bd = BdCli::configured(env.bd_conn_retries)?;
    send_under(&env, &bd, m)
}

/// [`send`] under `env` and `bd`: what `mail send` does after reading its arguments, the
/// attempt record included.
pub fn send_under(env: &Env, bd: &dyn Bd, m: &Message) -> Result<(), String> {
    let args = SendArgs {
        mailbox: m.mailbox,
        from: Some(m.from),
        subject: m.subject,
        kind: m.kind,
        default: m.default.unwrap_or(""),
        class: m.class.unwrap_or(""),
        bead: m.bead.unwrap_or(""),
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let result = mail::cmds::send(bd, env, &args, m.body.to_string());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(a) = mail::cmds::attempt_of_send(&args, m.body, &result, now) {
        if let Err(e) = a.write(&env.run_dir, &tsd::default_host()) {
            eprintln!("mail: send attempt not recorded: {e}");
        }
    }
    result.map(|_| ()).map_err(|e| format!("mail send {}: {e}", m.mailbox))
}

pub fn note_concierge(subject: &str, body: &str) -> Result<(), String> {
    send(&Message { mailbox: "concierge", from: FROM, subject, kind: "note", default: None, class: None, bead: None, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail::bead::BdOut;
    use std::path::Path;

    struct NoStore;

    impl Bd for NoStore {
        fn run(&self, args: &[String], _stdin: Option<&str>) -> BdOut {
            BdOut::fail(1, format!("no store in this test: {args:?}"))
        }
        fn close(&self, id: &str, _reason: &str) -> BdOut {
            BdOut::fail(1, format!("no store in this test: close {id}"))
        }
        fn ensure_ask(&self, id: &str, _work: &str) -> Result<(), String> {
            Err(format!("no store in this test: ask {id}"))
        }
    }

    fn env(root: &Path) -> Env {
        Env {
            mail_root: root.join("mail"),
            kinds_dir: root.join("kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 0,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: "needs-x".into(),
            db: String::new(),
            bd_conn_retries: 1,
            operator_actor: "op".into(),
            run_dir: root.join("run"),
            home: root.to_path_buf(),
            mail_from: None,
            lint_considered: Some("test".into()),
            repeat_considered: Some("test".into()),
            operator_considered: None,
            statute_guards: String::new(),
            statute_considered: None,
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
        }
    }

    #[test]
    fn a_note_is_delivered_into_the_mailbox_without_a_mail_program() {
        let t = testkit::TempDir::new("engine-mail-note");
        let e = env(t.path());
        let m = Message { mailbox: "concierge", from: FROM, subject: "RECONCILER: gap", kind: "note", default: None, class: None, bead: None, body: "## Note\nthe evidence\n" };
        send_under(&e, &NoStore, &m).unwrap();
        let new = t.path().join("mail/concierge/new");
        let files: Vec<_> = std::fs::read_dir(&new).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(text.contains("Subject: RECONCILER: gap") && text.contains("the evidence") && text.contains(FROM), "{text}");
    }

    #[test]
    fn a_refused_message_names_the_mailbox() {
        let t = testkit::TempDir::new("engine-mail-refused");
        let m = Message { mailbox: "--subject", from: FROM, subject: "s", kind: "note", default: None, class: None, bead: None, body: "b" };
        let err = send_under(&env(t.path()), &NoStore, &m).unwrap_err();
        assert!(err.starts_with("mail send --subject: "), "{err}");
    }
}
