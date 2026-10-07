//! [`Settler`] against the host: `bd`, the lifecycle machine, `git` and `systemctl`.


use crate::decide::Scope;
use crate::ports::Bd;
use crate::real::{json_only_pub, RealBd};
use crate::settle::{Candidate, Detector, FixState, Landing, Settler};

pub struct RealSettler<'a> {
    pub bd: &'a RealBd,
    pub db: String,
    pub incident_label: String,
    pub home_repo: String,
    pub registry: spira_config::repos::Registry,
    pub release_sha: Option<String>,
}

fn show(bd: &RealBd, db: &str, id: &str) -> Result<serde_json::Value, String> {
    let (rc, out, err) = bd.run(db, &["list", "--all", "--limit", "0", "--json", "--id", id])?;
    if rc != 0 {
        return Err(format!("bd list exited {rc}: {}", err.trim()));
    }
    let v: serde_json::Value = serde_json::from_str(json_only_pub(&out)).map_err(|e| e.to_string())?;
    v.as_array().and_then(|a| a.first().cloned()).ok_or_else(|| format!("{id} not found"))
}

pub fn blocks_of(bead: &serde_json::Value) -> Vec<String> {
    let id = bead.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut out: Vec<String> = bead
        .get("dependencies")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter(|d| d.get("dep_type").or_else(|| d.get("type")).and_then(|t| t.as_str()) == Some("blocks"))
                .filter(|d| d.get("issue_id").and_then(|i| i.as_str()).map_or(true, |i| i == id))
                .filter_map(|d| d.get("depends_on_id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// A commit-message pattern matching `id` as a whole bead id, not a prefix of another.
pub fn id_pattern(id: &str) -> String {
    format!("(^|[^A-Za-z0-9-]){}([^A-Za-z0-9.-]|$)", id.replace('.', "\\."))
}

fn git(root: &str, args: &[&str]) -> Result<(i32, String), String> {
    let out = spira_config::bounded::bounded("git").args(["-C", root]).args(args).output().map_err(|e| format!("git: {e}"))?;
    Ok((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

impl Settler for RealSettler<'_> {
    fn candidates(&self) -> Result<Vec<Candidate>, String> {
        let rows = self.bd.list(&self.db, Scope::Unfinished, Some(&self.incident_label), None)?;
        Ok(rows
            .into_iter()
            .filter(|r| r.status == crate::decide::BeadStatus::Open)
            .map(|r| Candidate { id: r.id, external_ref: r.external_ref, labels: r.labels })
            .collect())
    }

    fn fixes(&self, incident: &str) -> Result<Vec<String>, String> {
        Ok(blocks_of(&show(self.bd, &self.db, incident)?))
    }

    fn fix_state(&self, fix: &str) -> Result<FixState, String> {
        match spira_config::lc_state::row(fix)? {
            Some(r) if matches!(r.state.as_str(), "LANDED" | "DONE") => Ok(FixState::Landed),
            Some(r) => Ok(FixState::Unlanded(r.state)),
            None => Err("no lifecycle row".into()),
        }
    }

    fn landing_in_force(&self, fix: &str) -> Result<Option<Landing>, String> {
        let bead = show(self.bd, &self.db, fix)?;
        let repo = bead
            .get("labels")
            .and_then(|l| l.as_array())
            .and_then(|a| a.iter().filter_map(|l| l.as_str()).find_map(|l| l.strip_prefix("repo:")))
            .unwrap_or(&self.home_repo)
            .to_string();
        let root = self.registry.root(&repo).ok_or_else(|| format!("repo {repo} has no checkout"))?;
        let base = self.registry.base(&repo).filter(|b| !b.is_empty()).ok_or_else(|| format!("repo {repo} has no landing ref"))?;
        let (rc, sha) = git(&root, &["log", &base, "--format=%H", "-E", "--grep", &id_pattern(fix), "-n", "1"])?;
        if rc != 0 || sha.is_empty() {
            return Err(format!("no commit on {base} names {fix}"));
        }
        if repo != self.home_repo {
            return Ok(Some(Landing { sha }));
        }
        let release = self.release_sha.as_deref().ok_or("the active release is not a commit sha")?;
        match git(&root, &["merge-base", "--is-ancestor", &sha, release])?.0 {
            0 => Ok(Some(Landing { sha })),
            1 => Ok(None),
            rc => Err(format!("git merge-base exited {rc}")),
        }
    }

    fn detect(&self, c: &Candidate) -> Result<Detector, String> {
        let Some(unit) = c.external_ref.as_deref().and_then(|r| r.strip_prefix("incident:")) else { return Ok(Detector::NoProbe) };
        if ![".service", ".timer", ".socket", ".scope"].iter().any(|s| unit.ends_with(s)) {
            return Ok(Detector::NoProbe);
        }
        let out = spira_config::bounded::bounded("systemctl").args(["--user", "is-failed", unit]).output().map_err(|e| format!("systemctl: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(Detector::Firing(format!("{unit} is failed"))),
            Some(1) => Ok(Detector::Quiet),
            rc => Err(format!("systemctl is-failed exited {rc:?}")),
        }
    }

    fn close(&self, id: &str, evidence: &str) -> Result<(), String> {
        spira_config::lifecycle_row::close(id, evidence, "incident", None)
    }

    fn persist(&self, id: &str, label: &str, note: &str) -> Result<(), String> {
        if !self.bd.note(&self.db, id, note) {
            return Err("bd note failed".into());
        }
        if !self.bd.label_add(&self.db, id, label) {
            return Err("bd label add failed".into());
        }
        Ok(())
    }

    fn now_iso(&self) -> String {
        crate::run::iso_now_public(spira_config::vtime::now_epoch() as i64)
    }
}

/// The 40-hex directory name of `$SPIRA_RELEASE`, when it is one.
pub fn release_sha_of(release: &str) -> Option<String> {
    let name = std::path::Path::new(release.trim_end_matches('/')).file_name()?.to_str()?;
    (name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit())).then(|| name.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_of_keeps_only_this_beads_blocks_edges() {
        let b: serde_json::Value = serde_json::from_str(
            r#"{"id":"i","dependencies":[{"issue_id":"i","depends_on_id":"f","dep_type":"blocks"},
            {"issue_id":"i","depends_on_id":"r","dep_type":"relates-to"},{"issue_id":"x","depends_on_id":"o","dep_type":"blocks"}]}"#,
        )
        .unwrap();
        assert_eq!(blocks_of(&b), vec!["f"]);
    }

    #[test]
    fn the_id_pattern_does_not_match_a_longer_or_child_id() {
        let re = id_pattern("sp-ab.1");
        assert!(re.contains("sp-ab\\.1"));
        assert!(!re.contains("sp-ab.1"));
    }

    #[test]
    fn release_sha_is_the_forty_hex_directory_name() {
        let sha = "9f6f7e976db26649b7e4b753de0a992b1fc2f75b";
        assert_eq!(release_sha_of(&format!("/r/spira-releases/{sha}/")), Some(sha.to_string()));
        assert_eq!(release_sha_of("/r/checkout"), None);
    }
}
