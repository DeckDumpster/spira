use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{parse_stacked_id, Merge, Probe, INFRA_FAULT};

pub struct GitProbe {
    pub repo: PathBuf,
    /// This repo's registry name, and every registered name (this one included).
    pub name: String,
    pub registered: Vec<String>,
    /// `SPIRA_WIKI`; empty when unset.
    pub wiki: String,
    pub identity_projects: String,
    pub base_ref: String,
    pub work: PathBuf,
    #[allow(clippy::type_complexity)]
    pub state: Box<dyn Fn(&str) -> Result<String, String>>,
    #[allow(clippy::type_complexity)]
    pub shown: Box<dyn Fn(&str) -> Result<String, String>>,
}

fn git(dir: &Path) -> Command {
    let mut c = spira_config::bounded::bounded("git");
    c.arg("-C").arg(dir);
    c
}

fn out(c: &mut Command, what: &str) -> Result<String, String> {
    let o = c.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        return Err(format!("{what} exited {}: {}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

impl Probe for GitProbe {
    fn base(&self) -> Result<String, String> {
        out(git(&self.repo).args(["rev-parse", "--verify", &format!("{}^{{commit}}", self.base_ref)]), "git rev-parse").map(|s| s.trim().to_string())
    }

    fn merge(&self, tip: &str) -> Result<Merge, String> {
        let o = git(&self.repo)
            .args(["-c", &format!("attr.tree={}", self.base_ref), "merge-tree", "--write-tree", "--name-only", "--no-messages", &self.base_ref, tip])
            .output()
            .map_err(|e| format!("git merge-tree: {e}"))?;
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").to_string();
        let hex = first.len() >= 40 && first.bytes().all(|b| b.is_ascii_hexdigit());
        match o.status.code() {
            Some(1) if hex => Ok(Merge::Conflict(lines.take_while(|l| !l.is_empty()).map(str::to_string).collect())),
            Some(0) if hex => {
                let base = self.base()?;
                let c = out(git(&self.repo).args(["commit-tree", &first, "-p", &base, "-p", tip, "-m", "sift merge"]), "git commit-tree")?;
                Ok(Merge::Clean(c.trim().to_string()))
            }
            code => Err(format!("git merge-tree exited {code:?}: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").trim())),
        }
    }

    fn patch_id(&self, tip: &str) -> Result<String, String> {
        let diff = git(&self.repo).args(["diff", &format!("{}...{tip}", self.base_ref)]).output().map_err(|e| format!("git diff: {e}"))?;
        if !diff.status.success() {
            return Err("git diff failed".into());
        }
        let mut child = git(&self.repo).args(["patch-id", "--stable"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("git patch-id: {e}"))?;
        child.stdin.take().ok_or("no stdin")?.write_all(&diff.stdout).map_err(|e| format!("git patch-id: {e}"))?;
        let o = child.wait_with_output().map_err(|e| format!("git patch-id: {e}"))?;
        if !o.status.success() {
            return Err("git patch-id failed".into());
        }
        Ok(String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap_or("").to_string())
    }

    fn stacked_on(&self, id: &str, tip: &str) -> Result<Vec<String>, String> {
        let log = out(git(&self.repo).args(["log", "--format=%s", &format!("{}..{tip}", self.base_ref)]), "git log")?;
        let mut ids: Vec<String> = log.lines().filter_map(|s| parse_stacked_id(s, id)).collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn state(&self, id: &str) -> Result<String, String> {
        (self.state)(id)
    }

    fn target_gap(&self, id: &str) -> Result<Option<String>, String> {
        (self.shown)(id).map(|text| spira_config::target::gap(&text))
    }

    fn identity_gap(&self, tip: &str) -> Result<Option<String>, String> {
        if !self.registered.iter().any(|r| *r != self.name) {
            return Ok(None);
        }
        let pages = crate::identity::load(&self.wiki, &self.identity_projects, &self.registered)?;
        let diff = out(git(&self.repo).args(["diff", "-U0", "--no-color", "--no-ext-diff", &format!("{}...{tip}", self.base_ref)]), "git diff").map_err(|e| format!("{INFRA_FAULT}{e}"))?;
        Ok(crate::identity::gap(&self.name, &pages, &Path::new(&self.wiki).join(&self.identity_projects), &diff))
    }
}
