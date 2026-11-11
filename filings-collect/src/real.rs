use crate::{Filing, Host, World};
use spira_config::bounded::bounded_for;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static BODY_SEQ: AtomicUsize = AtomicUsize::new(0);

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];
const PERSONA: &str = "builder";

pub struct Real {
    pub ssh: String,
    pub bead: String,
    client: spira_config::lc_call::LcClient,
}

impl Real {
    pub fn new() -> Result<Real, String> {
        Ok(Self::over(spira_config::lc_call::LcClient::configured()?))
    }

    /// Store reads through `client`.
    pub fn over(client: spira_config::lc_call::LcClient) -> Real {
        Real { ssh: "ssh".into(), bead: "bead.sh".into(), client }
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn run(mut c: spira_config::bounded::Bounded) -> Result<String, String> {
    let out = c.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

impl Real {
    fn remote(&self, h: &Host, script: &str) -> Result<String, String> {
        let mut c = bounded_for(&self.ssh, "60");
        c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", &h.ssh, script]);
        run(c)
    }

    fn bead(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
        // FORK-EDGE: content store write (bead.sh file/amend, the operator filing tool)
        let mut c = bounded_for(&self.bead, "120");
        c.args(args);
        run(c)
    }

    fn with_body<T>(&self, body: &str, f: impl FnOnce(&str) -> Result<T, String>) -> Result<T, String> {
        let p: PathBuf = std::env::temp_dir().join(format!("filings-collect-{}-{}.md", std::process::id(), BODY_SEQ.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        let r = f(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        r
    }
}

impl World for Real {
    fn list(&self, h: &Host) -> Result<Vec<String>, String> {
        let home = sh_quote(&h.home);
        let script = format!("test -d {home} || {{ echo 'no such directory' >&2; exit 3; }}; cd {home}/filings/outbox 2>/dev/null || exit 0; ls -1");
        Ok(self.remote(h, &script)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }

    fn read(&self, h: &Host, name: &str) -> Result<String, String> {
        self.remote(h, &format!("cat -- {}", sh_quote(&format!("{}/filings/outbox/{name}", h.home))))
    }

    fn ack(&self, h: &Host, name: &str) -> Result<(), String> {
        let (acked, from) = (format!("{}/filings/acked", h.home), format!("{}/filings/outbox/{name}", h.home));
        self.remote(h, &format!("mkdir -p -- {0} && mv -- {1} {0}/", sh_quote(&acked), sh_quote(&from))).map(|_| ())
    }

    fn is_open(&self, id: &str) -> Result<bool, String> {
        let out = self.client.call(&["show", id]).map_err(String::from)?;
        match out.code {
            1 => Ok(false),
            0 => {
                let doc: serde_json::Value = serde_json::from_str(&out.out).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok(!TERMINAL.contains(&state))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", out.out.trim())),
        }
    }

    fn file(&self, f: &Filing, body: &str) -> Result<String, String> {
        let out = self.with_body(body, |p| self.bead(&["file", &f.title, "--for", PERSONA, "--repo", &f.repo, "--body-file", p]))?;
        out.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from).ok_or_else(|| "bead.sh printed no id".into())
    }

    fn append(&self, id: &str, body: &str, note: &str) -> Result<(), String> {
        self.with_body(body, |p| self.bead(&["amend", id, "--append-body", p, "--note", note])).map(|_| ())
    }
}

#[cfg(test)]
mod store_client_tests {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    /// `show` answered as the store answers it: a row with its state, or exit 1 for none.
    pub struct Scripted(pub Mutex<Vec<String>>);

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let reply = |code, out: &str| Ok(Reply { code, out: out.into(), err: String::new() });
            match argv.get(1).map(String::as_str) {
                Some("sp-open") => reply(0, r#"{"bead":{"state":"READY"}}"#),
                Some("sp-done") => reply(0, r#"{"bead":{"state":"LANDED"}}"#),
                Some("sp-gone") => reply(1, "{}"),
                _ => reply(2, "cannot tell: down"),
            }
        }
    }

    pub fn client() -> (LcClient, Arc<Scripted>) {
        let t = Arc::new(Scripted(Mutex::default()));
        (LcClient::over(t.clone(), true), t)
    }

    #[test]
    fn whether_a_filing_is_open_is_read_through_the_store_client() {
        let (lc, t) = client();
        let real = Real::over(lc);
        assert_eq!(real.is_open("sp-open"), Ok(true));
        assert_eq!(real.is_open("sp-done"), Ok(false));
        assert_eq!(real.is_open("sp-gone"), Ok(false));
        assert!(real.is_open("sp-down").is_err());
        assert_eq!(t.0.lock().unwrap().len(), 4);
    }
}
