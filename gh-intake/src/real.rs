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

/// Sources `lib.sh` (and, transitively, `conf.sh`) exactly once per call and asks it a single
/// question — the same boundary gate-check's Rust port already draws around this same
/// function (`repo_root`, unported; spira/lib.sh is last in the rewrite order).
pub struct RealRepo {
    pub spira_home: String,
}

impl Repo for RealRepo {
    fn root_with_git(&self, name: &str) -> Option<String> {
        let script = ". \"$0\" >/dev/null 2>&1 || exit 96\nrepo_root \"$1\" 2>/dev/null";
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(format!("{}/lib.sh", self.spira_home))
            .arg(name)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if root.is_empty() {
            return None;
        }
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
