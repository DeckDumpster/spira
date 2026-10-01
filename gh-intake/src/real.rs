use crate::ports::{Bd, Http, Mail, Repo};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct RealHttp;

impl Http for RealHttp {
    fn get(&self, url: &str, timeout_secs: u64) -> Result<(u16, Vec<u8>), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(timeout_secs))
            .build();
        match agent.get(url).call() {
            Ok(resp) => {
                let status = resp.status();
                let mut buf = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                Ok((status, buf))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let mut buf = Vec::new();
                let _ = resp.into_reader().read_to_end(&mut buf);
                Ok((code, buf))
            }
            Err(ureq::Error::Transport(t)) => Err(t.to_string()),
        }
    }
}

use std::io::Read;

pub struct RealBd {
    pub bd_bin: String,
    pub db: String,
}

impl RealBd {
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bd_bin);
        c.arg("-C").arg(&self.db);
        c
    }
}

impl Bd for RealBd {
    fn list_all_json(&self) -> Option<serde_json::Value> {
        let out = self
            .cmd()
            .args(["list", "--all", "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn create(&self, title: &str, external_ref: &str, labels: &str, priority: &str, body: &[u8]) -> bool {
        let mut child = match self
            .cmd()
            .arg("create")
            .arg(title)
            .arg("--external-ref")
            .arg(external_ref)
            .arg("--labels")
            .arg(labels)
            .arg("-t")
            .arg("bug")
            .arg("-p")
            .arg(priority)
            .arg("--body-file")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        matches!(child.wait(), Ok(status) if status.success())
    }

    fn note(&self, id: &str, text: &str) -> bool {
        let status = self
            .cmd()
            .arg("note")
            .arg(id)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }

    fn close(&self, id: &str, reason: &str) -> bool {
        let status = self
            .cmd()
            .arg("close")
            .arg(id)
            .arg("--reason")
            .arg(reason)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }
}

/// `repo_root` (family U), in-process via `spira_config::repos` (sp-k6lku, "wave 4.13") —
/// no longer a `bash -c '. lib.sh; ...'` seam at all.
pub struct RealRepo {
    pub spira_home: String,
}

impl Repo for RealRepo {
    /// `repo_root <name>` (family U) in-process now (sp-k6lku, "wave 4.13") through
    /// `spira_config::repos::Registry::from_env` — the one production door onto a
    /// registry (following the structural fix for sp-z3eyk): building one from a bare
    /// `std::env::vars()` directly, with no resolution, found no map at all in production,
    /// since conf.sh exports none of `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED`; `from_env` resolves them in-process instead. No
    /// `bash -c '. lib.sh; repo_root ...'` subprocess at all (gh-intake is a one-shot
    /// binary, so there is no loop to amortise a cached Registry over).
    fn root_with_git(&self, name: &str) -> Option<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        let root = reg.root(name)?;
        if std::path::Path::new(&root).join(".git").exists() {
            Some(root)
        } else {
            None
        }
    }
}

pub struct RealMail {
    pub mail_bin: String,
}

impl Mail for RealMail {
    fn send_operator_note(&self, subject: &str, body: &[u8]) -> bool {
        let mut child = match Command::new(&self.mail_bin)
            .args(["send", "operator", "--from", "gh-intake <intake@spira>", "--subject", subject, "--kind", "note"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        matches!(child.wait(), Ok(status) if status.success())
    }
}
