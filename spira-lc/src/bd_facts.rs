//! Gathers the classifier's bd-derived facts (status, labels, a `supersedes` dependency) by
//! shelling out to a real `bd` — the real dependency, not a hand-modelled JSON shape
//! (`law-prefer-the-real-dependency`): a stub reproduces the surface remembered at the time
//! it was written, and bd's own JSON shape is exactly the kind of surface that drifts.

use std::collections::BTreeSet;
use std::process::Command;

use lifecycle::classify::BdStatus;

pub struct BdRecord {
    pub status: BdStatus,
    pub labels: BTreeSet<String>,
    pub supersedes: Option<String>,
    pub is_epic: bool,
    pub external: bool,
}

/// Runs `bd -C <db> show <id> --json` and extracts exactly the fields the classifier reads.
pub fn fetch(bd_bin: &str, db: &str, id: &str) -> Result<BdRecord, String> {
    let out = Command::new("timeout")
        .args(["5", bd_bin, "-C", db, "show", id, "--json"])
        .output()
        .map_err(|e| format!("spawning {bd_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("bd show {id}: {}", String::from_utf8_lossy(&out.stderr)));
    }
    parse(&String::from_utf8_lossy(&out.stdout), id)
}

fn parse(text: &str, id: &str) -> Result<BdRecord, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("bd show {id}: {e}: {text}"))?;
    let items = value.as_array().cloned().unwrap_or_else(|| vec![value.clone()]);
    let item = items.first().ok_or_else(|| format!("bd show {id}: no issue returned"))?;
    if let Some(err) = item.get("error").and_then(|v| v.as_str()) {
        return Err(format!("bd show {id}: {err}"));
    }
    let status_str = item.get("status").and_then(|v| v.as_str()).unwrap_or("open");
    let status = match status_str {
        "open" => BdStatus::Open,
        "in_progress" => BdStatus::InProgress,
        "closed" => BdStatus::Closed,
        other => return Err(format!("bd show {id}: unrecognized status {other:?}")),
    };
    let labels: BTreeSet<String> = item
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();
    let supersedes = item
        .get("dependencies")
        .and_then(|v| v.as_array())
        .and_then(|deps| deps.iter().find(|d| d.get("dependency_type").and_then(|t| t.as_str()) == Some("supersedes")))
        .and_then(|d| d.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let is_epic = item.get("issue_type").and_then(|v| v.as_str()) == Some("epic");
    let external = item.get("external_ref").and_then(|v| v.as_str()).is_some_and(|r| !r.trim().is_empty());
    Ok(BdRecord { status, labels, supersedes, is_epic, external })
}

/// Lists every bead id labelled `repo:<name>` via `bd list --json --all`, scoped exactly the
/// way the rest of the harness already scopes work to a repository. `bd show` (via [`fetch`])
/// supplies every other fact, one call per bead — acceptable for a one-time migration tool:
/// design Intent 5's "cheap, fast" bench is about the lifecycle machine's own transitions,
/// never about this tool, which runs once against a drained store.
pub fn roster(bd_bin: &str, db: &str, repo_name: &str) -> Result<Vec<String>, String> {
    list_ids(bd_bin, db, &["--label", &format!("repo:{repo_name}")], "5")
}

/// Every bead id in the database, in any status and with any labels — install's one-time
/// population (sp-k62xz8) gives each a lifecycle row, so none may be left off this roster by
/// a label filter. Bounded longer than [`roster`]: one unfiltered list of a whole store.
pub fn roster_all(bd_bin: &str, db: &str) -> Result<Vec<String>, String> {
    list_ids(bd_bin, db, &[], "30")
}

fn list_ids(bd_bin: &str, db: &str, filter: &[&str], secs: &str) -> Result<Vec<String>, String> {
    let out = Command::new("timeout")
        .args([secs, bd_bin, "-C", db, "list", "--json", "--all"])
        .args(filter)
        .args(["--limit", "0"])
        .output()
        .map_err(|e| format!("spawning {bd_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("bd list: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bd list: {e}"))?;
    let items = value.as_array().cloned().unwrap_or_default();
    Ok(items.iter().filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_external_ref_marks_the_record_external() {
        let rec = parse(r#"[{"id": "sp-1", "status": "closed", "external_ref": "github:o/r#4"}]"#, "sp-1").unwrap();
        assert!(rec.external);
        assert!(!parse(r#"[{"id": "sp-1", "status": "closed"}]"#, "sp-1").unwrap().external);
    }

    #[test]
    fn an_error_response_is_refused_not_defaulted_to_open() {
        let text = r#"{"error": "no issues found matching the provided IDs"}"#;
        assert!(parse(text, "sp-missing").is_err());
    }
}
