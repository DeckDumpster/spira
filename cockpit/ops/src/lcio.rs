//! The cockpit's in-process doors: the store's served verbs over [`LcClient`] (no `spira-lc`
//! process per read), and mail delivered through the `mail` library (no `mail sendmail`
//! process). Each answer keeps the text the child used to print, so a pane reads the same.

use std::path::Path;

use spira_config::lc_call::{LcClient, Reply};

/// The configured store client: the declared socket, with the release's `spira-lc` behind it
/// only for the verbs the CLI answers.
pub fn client() -> Result<LcClient, String> {
    LcClient::configured()
}

fn last_line(r: &Reply) -> String {
    let text = if r.err.trim().is_empty() { &r.out } else { &r.err };
    text.lines().last().unwrap_or("").to_string()
}

/// What a read printed, when it exited 0; otherwise `spira-lc <argv>: exit <code> <last line>`.
pub fn answer(c: &LcClient, argv: &[&str]) -> Result<String, String> {
    let r = c.call(argv).map_err(|e| format!("spira-lc {}: {e}", argv.join(" ")))?;
    if r.code != 0 {
        return Err(format!("spira-lc {}: exit {} {}", argv.join(" "), r.code, last_line(&r)));
    }
    Ok(r.out)
}

/// Whether `id` has a lifecycle row: `show` answers 0 for a row and 1 for none.
pub fn has_row(c: &LcClient, id: &str) -> Result<bool, String> {
    let r = c.call(&["show", id]).map_err(|e| format!("running spira-lc: {e}"))?;
    match r.code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!("spira-lc show exit Some({code}): {}", if r.err.is_empty() { &r.out } else { &r.err })),
    }
}

/// One call's exit code and everything it printed, stdout first (what `run_bounded` returned);
/// no answer at all is "cannot tell" (2).
pub fn code_and_text(c: &LcClient, argv: &[&str]) -> (i32, String) {
    match c.call(argv) {
        Ok(r) => (r.code, format!("{}{}", r.out, r.err)),
        Err(e) => (2, e.to_string()),
    }
}

/// `mail sendmail [--to <mailbox>]`, in process: spooled first, delivered, the spool removed on
/// success and named on failure — the command's own sequence.
pub fn sendmail(raw: &str, to: Option<&str>) -> Result<(), String> {
    let env = mail::env::Env::load().map_err(|e| format!("mail: {e}"))?;
    deliver(&env.home, &env.mail_root, &env.run_dir, env.mute, raw, to)
}

pub fn deliver(home: &Path, mail_root: &Path, run_dir: &Path, mute: bool, raw: &str, to: Option<&str>) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("mail: sendmail: empty message — nothing to deliver".into());
    }
    let spool = mail::sendmail::spool_message(run_dir, raw);
    match mail::sendmail::sendmail_to(home, mail_root, mute, raw, to) {
        Ok(_) => {
            if let Some(p) = spool {
                let _ = std::fs::remove_file(p);
            }
            Ok(())
        }
        Err(e) => {
            let kept = spool.map(|p| format!(" (message kept at {})", p.display())).unwrap_or_default();
            Err(format!("mail: sendmail: {e}{kept}"))
        }
    }
}

/// A scripted store for tests: records every argv and answers each with one reply.
#[cfg(test)]
pub mod scripted {
    use super::*;
    use spira_config::lc_call::Transport;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub struct Scripted {
        pub calls: Mutex<Vec<Vec<String>>>,
        pub reply: Reply,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.calls.lock().unwrap().push(argv.to_vec());
            Ok(self.reply.clone())
        }
    }

    pub fn client(code: i32, out: &str) -> (Arc<Scripted>, LcClient) {
        let t = Arc::new(Scripted { calls: Mutex::new(Vec::new()), reply: Reply { code, out: out.into(), err: String::new() } });
        (Arc::clone(&t), LcClient::over(t, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_answers_in_process_and_a_failure_names_the_call() {
        let (t, ok) = scripted::client(0, "[]");
        assert_eq!(answer(&ok, &["ops-graph"]).unwrap(), "[]");
        assert_eq!(*t.calls.lock().unwrap(), vec![vec!["ops-graph".to_string()]]);
        let (_, no) = scripted::client(2, "cannot tell: down");
        assert_eq!(answer(&no, &["list", "--live"]).unwrap_err(), "spira-lc list --live: exit 2 cannot tell: down");
    }

    #[test]
    fn has_row_reads_show_exit_codes() {
        assert!(has_row(&scripted::client(0, "{}").1, "sp-a").unwrap());
        assert!(!has_row(&scripted::client(1, "").1, "sp-a").unwrap());
        assert!(has_row(&scripted::client(2, "cannot tell").1, "sp-a").is_err());
    }

    #[test]
    fn deliver_lands_the_message_in_the_concierge_mailbox_and_removes_the_spool() {
        let dir = testkit::TempDir::new("ops-deliver");
        let (home, root, run) = (dir.join("home"), dir.join("mail"), dir.join("run"));
        for d in [&home, &root, &run] {
            std::fs::create_dir_all(d).unwrap();
        }
        deliver(&home, &root, &run, false, "From: op <op@spira>\nSubject: s\n\nbody\n", None).unwrap();
        assert_eq!(std::fs::read_dir(root.join("concierge/new")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(run.join("spira-sendmail")).unwrap().count(), 0);
    }
}
