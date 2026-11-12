//! world's mail through the `mail` library in this process: what a
//! `SPIRA_MAIL_LINT_CONSIDERED=1 mail send <box> --from "world <world@spira>" …` child did, with
//! the same delivery and the same recorded attempt.

use mail::bead::Bd;
use mail::cmds::SendArgs;
use mail::env::Env;

pub const FROM: &str = "world <world@spira>";

/// One letter: `body` to `mailbox` under `subject`, `kind` empty when the old call passed none.
pub struct Letter<'a> {
    pub mailbox: &'a str,
    pub subject: &'a str,
    pub kind: &'a str,
    pub body: &'a str,
}

/// Send `letter` with `bd` and `env`, lint acknowledged as the old child's environment did.
pub fn send(bd: &dyn Bd, env: &Env, letter: &Letter) -> bool {
    let env = Env { lint_considered: Some("1".into()), ..env.clone() };
    let args = SendArgs {
        mailbox: letter.mailbox,
        from: Some(FROM),
        subject: letter.subject,
        kind: letter.kind,
        default: "",
        class: "",
        bead: "",
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let result = mail::cmds::send(bd, &env, &args, letter.body.to_string());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(a) = mail::cmds::attempt_of_send(&args, letter.body, &result, now) {
        let _ = a.write(&env.run_dir, &tsd::default_host());
    }
    result.is_ok()
}

/// [`send`] with the configured store and environment, as the `mail` binary builds them.
pub fn send_configured(letter: &Letter) -> bool {
    let Ok(env) = Env::load() else { return false };
    let Ok(bdq) = spira_config::release_env::tool("bdq") else { return false };
    let bd = mail::bead::BdCli { bdq, conn_retries: env.bd_conn_retries };
    send(&bd, &env, letter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail::bead::BdOut;
    use std::path::{Path, PathBuf};

    struct NoStore;

    impl Bd for NoStore {
        fn run(&self, _args: &[String], _stdin: Option<&str>) -> BdOut {
            BdOut::ok("")
        }
        fn close(&self, _id: &str, _reason: &str) -> Result<(), spira_store::lc_call::LcError> {
            Ok(())
        }
        fn ensure_ask(&self, _id: &str, _work_bead: &str) -> Result<(), String> {
            Ok(())
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
            ask_label: String::new(),
            db: String::new(),
            bd_conn_retries: 1,
            operator_actor: "operator".into(),
            run_dir: root.join("run"),
            home: PathBuf::from(root),
            mail_from: None,
            // Unset here: `send` must acknowledge the lint itself, as the old child's env did.
            lint_considered: None,
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
    fn a_dwarf_notice_lands_in_its_mailbox_from_world_without_a_kinds_dir() {
        let tmp = testkit::TempDir::new("world-mail");
        std::fs::create_dir_all(tmp.path().join("run")).unwrap();
        std::fs::create_dir_all(tmp.path().join("mail/dwarf-sp-a/new")).unwrap();
        let letter = Letter { mailbox: "dwarf-sp-a", subject: "Deadline: wrap up or checkpoint", kind: "", body: "wrap up\n" };
        assert!(send(&NoStore, &env(tmp.path()), &letter));
        let files: Vec<_> = std::fs::read_dir(tmp.path().join("mail/dwarf-sp-a/new")).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(text.contains("From: world <world@spira>") && text.contains("wrap up"), "{text}");
    }
}
