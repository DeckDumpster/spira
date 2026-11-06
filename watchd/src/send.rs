//! Mail from watchd through the `mail` library in this process — what a `mail send <box>`
//! child did, with the same arguments, the same delivery and the same recorded attempt.

use mail::bead::Bd;
use mail::cmds::SendArgs;
use mail::env::Env;

pub const FROM: &str = "Watchd <watchd@spira>";

/// One `mail send <mailbox> --from FROM --subject … --kind … [--default …]` with `body` on stdin.
pub struct Letter<'a> {
    pub mailbox: &'a str,
    pub subject: &'a str,
    pub kind: &'a str,
    pub default: &'a str,
    pub body: &'a str,
}

/// Send `letter` with `bd` and `env`; the error is what `mail` would have printed.
pub fn send(bd: &dyn Bd, env: &Env, letter: &Letter) -> Result<(), String> {
    let args = SendArgs {
        mailbox: letter.mailbox,
        from: Some(FROM),
        subject: letter.subject,
        kind: letter.kind,
        default: letter.default,
        class: "",
        bead: "",
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let result = mail::cmds::send(bd, env, &args, letter.body.to_string());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(a) = mail::cmds::attempt_of_send(&args, letter.body, &result, now) {
        if let Err(e) = a.write(&env.run_dir, &tsd::default_host()) {
            eprintln!("mail: send attempt not recorded: {e}");
        }
    }
    result.map(|_| ()).map_err(|e| {
        eprintln!("mail: send: {e}");
        format!("mail: send: {e}")
    })
}

/// [`send`] with the configured store and environment, as the `mail` binary builds them.
pub fn send_configured(letter: &Letter) -> Result<(), String> {
    let env = Env::load().map_err(|e| format!("mail: {e}"))?;
    let bd = mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: env.bd_conn_retries };
    send(&bd, &env, letter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail::bead::BdOut;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Default)]
    struct NoStore {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Bd for NoStore {
        fn run(&self, args: &[String], _stdin: Option<&str>) -> BdOut {
            self.calls.borrow_mut().push(args.to_vec());
            BdOut::ok("")
        }
        fn close(&self, _id: &str, _reason: &str) -> BdOut {
            BdOut::ok("")
        }
        fn ensure_ask(&self, _id: &str, _work_bead: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn env(root: &std::path::Path) -> Env {
        Env {
            mail_root: root.join("mail"),
            kinds_dir: root.join("kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 0,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: String::new(),
            db: String::new(),
            bd_conn_retries: 1,
            operator_actor: "operator".into(),
            run_dir: root.join("run"),
            home: PathBuf::from(root),
            mail_from: None,
            lint_considered: Some("test".into()),
            repeat_considered: None,
            operator_considered: None,
            statute_guards: String::new(),
            statute_considered: None,
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
        }
    }

    #[test]
    fn a_concierge_letter_lands_in_the_concierge_maildir_from_watchd() {
        let tmp = testkit::TempDir::new("watchd-send");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("run")).unwrap();
        let bd = NoStore::default();
        let letter = Letter { mailbox: "concierge", subject: "watcher down", kind: "incident", default: "", body: "the body\n" };
        send(&bd, &env(&root), &letter).unwrap();
        let new = root.join("mail/concierge/new");
        let files: Vec<_> = std::fs::read_dir(&new).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(text.contains("From: Watchd <watchd@spira>") && text.contains("Subject: watcher down") && text.contains("the body"), "{text}");
    }
}
