use crate::ports::{Bd, Gh, Git, Http, Mail, Repo};
use std::io::Write;
use std::path::Path;
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

    fn show_json(&self, id: &str) -> Option<serde_json::Value> {
        let out = self.cmd().arg("show").arg(id).arg("--json").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn list_by_label(&self, status: &str, label: &str) -> Option<serde_json::Value> {
        let out = self
            .cmd()
            .args(["list", "--status", status, "--label", label, "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn dep_remove(&self, id: &str, other: &str) -> bool {
        let status = self.cmd().args(["dep", "remove", id, other]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        matches!(status, Ok(s) if s.success())
    }

    fn dep_relate(&self, from: &str, to: &str) -> bool {
        let status = self.cmd().args(["dep", "relate", from, to]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
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

    /// `spira_repos` (sp-j3fim) — in-process, same registry construction as every other
    /// call here (one-shot binary, no loop to amortise it over).
    fn all_names(&self) -> Vec<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        reg.all()
    }

    /// `spira_landref <repo-path>` (family W, sp-j3fim) — in-process.
    fn landref(&self, repo_path: &str) -> Option<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        spira_config::repos::landref(&reg, repo_path)
    }

    /// `spira_landrefs <repo-path>` (family W, sp-j3fim) — in-process; flattens the
    /// (base, local-counterpart) pair `spira_config::repos::landrefs` returns into the
    /// list of refs `Git::landed_sha` greps.
    fn landrefs(&self, repo_path: &str) -> Vec<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        match spira_config::repos::landrefs(&reg, repo_path) {
            Some((base, Some(local))) => vec![base, local],
            Some((base, None)) => vec![base],
            None => Vec::new(),
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

    /// lib.sh `gh_issue_ask_unlanded`'s mail call (sp-j3fim): stdout discarded, stderr
    /// captured as the error text exactly as `_err="$(... 2>&1 >/dev/null)"` did.
    fn send_question(&self, from: &str, subject: &str, default: &str, body: &[u8]) -> Result<(), String> {
        let mut child = Command::new(&self.mail_bin)
            .args(["send", "operator", "--from", from, "--subject", subject, "--kind", "question", "--default", default])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

/// `ghq` is NOT its own binary — lib.sh's shim is `ghq() { command bdq __ghq "$@"; }`,
/// and that shim was never ported to a standalone executable (bead/src/bin/bdq.rs's
/// `cmd_ghq` is reached only through `bdq __ghq ...`). This execs `bdq` with that
/// internal subcommand prepended, by bare name on the launcher PATH, the same door every
/// bash family already shells through for `gh` access (sp-j3fim).
pub struct RealGh {
    pub bdq_bin: String,
}

impl RealGh {
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bdq_bin);
        c.arg("__ghq");
        c
    }
}

impl Gh for RealGh {
    fn issue_state(&self, repo: &str, issue_n: &str) -> String {
        let out = self
            .cmd()
            .args(["issue", "view", issue_n, "--repo", repo, "--json", "state", "-q", ".state"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => String::new(),
        }
    }
    fn issue_comment(&self, repo: &str, issue_n: &str, body: &str) -> bool {
        let status = self
            .cmd()
            .args(["issue", "comment", issue_n, "--repo", repo, "--body", body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }
    fn issue_close(&self, repo: &str, issue_n: &str) -> bool {
        let status = self
            .cmd()
            .args(["issue", "close", issue_n, "--repo", repo])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }
}

/// Plain git (sp-j3fim) — every call reads a checkout already on disk; no credential.
pub struct RealGit;

impl RealGit {
    fn cmd(repo: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(repo);
        c
    }
}

impl Git for RealGit {
    fn rev_parse_short(&self, repo: &Path, sha: &str) -> Option<String> {
        let o = Self::cmd(repo).args(["rev-parse", "--short", sha]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !o.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn subject_of(&self, repo: &Path, sha: &str) -> Option<String> {
        let o = Self::cmd(repo).args(["log", "--format=%s", "-1", sha]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn landed_sha(&self, repo: &Path, id: &str, refs: &[String]) -> Option<String> {
        if refs.is_empty() {
            return None;
        }
        let grep = format!("--grep={id}");
        let mut args: Vec<&str> = vec!["log", "--format=%H%x09%s", grep.as_str(), "-F"];
        args.extend(refs.iter().map(String::as_str));
        let o = Self::cmd(repo).args(&args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !o.status.success() {
            return None;
        }
        let out = String::from_utf8_lossy(&o.stdout);
        let land = format!("spira: land {id}");
        let own = format!("{id}:");
        for line in out.lines() {
            let Some((sha, subj)) = line.split_once('\t') else { continue };
            if subj == land || subj.starts_with(&format!("{land} ")) || subj.starts_with(&own) {
                return Some(sha.to_string());
            }
        }
        None
    }

    fn grep_ancestor(&self, repo: &Path, id: &str, land_ref: &str) -> Option<String> {
        let grep = format!("--grep={id}");
        let o = Self::cmd(repo)
            .args(["log", "--format=%H", grep.as_str(), land_ref])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_string).filter(|s| !s.is_empty())
    }

    fn sha_is_ancestor(&self, repo: &Path, sha: &str, land_ref: &str) -> bool {
        let exists = Self::cmd(repo).args(["cat-file", "-e", sha]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !exists {
            return false;
        }
        Self::cmd(repo)
            .args(["merge-base", "--is-ancestor", sha, land_ref])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
