//! The panel's in-process doors: the store's served verbs over [`LcClient`] (no `spira-lc`
//! process per call), and mail delivered through the `mail` library (no `mail sendmail`
//! process). What each answers on failure keeps the text the child used to print, so the
//! footer reads the same.

use std::path::Path;

use spira_store::lc_call::{is_refusal, LcClient, Reply};

use crate::store::Cfg;

/// The store client for this panel: the socket the config declares, with the release's
/// `spira-lc` behind it for the verbs only the CLI answers.
pub fn client(cfg: &Cfg) -> Result<LcClient, String> {
    // FORK-EDGE: LcClient's CLI route (direct mode and its CLI-answered verbs) runs this binary.
    LcClient::for_bin(&crate::store::bin("spira-lc", cfg))
}

fn first_line(r: &Reply) -> String {
    let line = |t: &str| t.lines().find(|l| !l.trim().is_empty()).map(str::to_string);
    line(&r.err).or_else(|| (r.code != 0).then(|| line(&r.out)).flatten()).unwrap_or_else(|| "failed".into())
}

/// A read: what was printed, unless it failed having printed nothing (a refusal is nothing:
/// the CLI printed it to stderr).
pub fn read(c: &LcClient, argv: &[&str]) -> Result<String, String> {
    let r = c.call(argv).map_err(|e| format!("spira-lc: {e}"))?;
    let printed_nothing = r.out.is_empty() || is_refusal(r.code, &r.out);
    if r.code != 0 && printed_nothing {
        return Err(format!("spira-lc: {}", first_line(&r)));
    }
    Ok(r.out)
}

/// A write: applied (exit 0), or the first line of why not.
pub fn write(c: &LcClient, argv: &[&str]) -> Result<(), String> {
    let r = c.call(argv).map_err(|e| format!("spira-lc: {e}"))?;
    if r.code != 0 {
        return Err(format!("spira-lc: {}", first_line(&r)));
    }
    Ok(())
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

#[cfg(test)]
pub mod tests {
    use super::*;
    use spira_store::lc_call::Transport;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A scripted store: records every argv and answers each with `reply`.
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

    pub fn scripted(code: i32, out: &str) -> (Arc<Scripted>, LcClient) {
        let t = Arc::new(Scripted { calls: Mutex::new(Vec::new()), reply: Reply { code, out: out.into(), err: String::new() } });
        (Arc::clone(&t), LcClient::over(t, true))
    }

    #[test]
    fn a_read_returns_the_answer_and_a_socket_refusal_is_the_error() {
        let (_, ok) = scripted(0, "{\"conditions\":[]}");
        assert_eq!(read(&ok, &["condition", "list"]).unwrap(), "{\"conditions\":[]}");
        let (_, refused) = scripted(2, "cannot tell: the store is down");
        assert_eq!(read(&refused, &["condition", "list"]).unwrap_err(), "spira-lc: cannot tell: the store is down");
    }

    #[test]
    fn a_write_fails_with_the_first_line_of_why() {
        let (_, no) = scripted(3, "refused: not firing\nmore");
        assert_eq!(write(&no, &["condition", "ack", "x"]).unwrap_err(), "spira-lc: refused: not firing");
        let (_, yes) = scripted(0, "");
        assert!(write(&yes, &["condition", "ack", "x"]).is_ok());
    }

    #[test]
    fn deliver_lands_the_message_in_the_concierge_mailbox_and_removes_the_spool() {
        let dir = testkit::TempDir::new("panel-deliver");
        let (home, root, run) = (dir.join("home"), dir.join("mail"), dir.join("run"));
        for d in [&home, &root, &run] {
            std::fs::create_dir_all(d).unwrap();
        }
        deliver(&home, &root, &run, false, "From: op <op@spira>\nSubject: s\n\nbody\n", None).unwrap();
        let landed: Vec<_> = std::fs::read_dir(root.join("concierge/new")).unwrap().flatten().collect();
        assert_eq!(landed.len(), 1);
        assert_eq!(std::fs::read_dir(run.join("spira-sendmail")).unwrap().count(), 0);
    }
}
