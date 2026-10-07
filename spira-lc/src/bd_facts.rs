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
    Ok(BdRecord { status, labels, supersedes })
}

/// Lists every bead id labelled `repo:<name>` via `bd list --json --all`, scoped exactly the
/// way the rest of the harness already scopes work to a repository. `bd show` (via [`fetch`])
/// supplies every other fact, one call per bead — acceptable for a one-time migration tool:
/// design Intent 5's "cheap, fast" bench is about the lifecycle machine's own transitions,
/// never about this tool, which runs once against a drained store.
pub fn roster(bd_bin: &str, db: &str, repo_name: &str) -> Result<Vec<String>, String> {
    let out = Command::new("timeout")
        .args(["5", bd_bin, "-C", db, "list", "--json", "--all", "--label", &format!("repo:{repo_name}"), "--limit", "0"])
        .output()
        .map_err(|e| format!("spawning {bd_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("bd list: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bd list: {e}"))?;
    let items = value.as_array().cloned().unwrap_or_default();
    Ok(items.iter().filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect())
}

/// `(id, close_reason)` for each of `ids` bd reports closed — reconcile-closed's read
/// (sp-bc0rlt, sp-3fue0j): like the classifier, it reconciles the machine from bd once, for
/// the rows a raw `bd close` left READY before every close went through `spira-lc close`.
pub fn closed(bd_bin: &str, db: &str, ids: &[String]) -> Result<Vec<(String, String)>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("timeout");
    cmd.args(["5", bd_bin]);
    if !db.is_empty() {
        cmd.args(["-C", db]);
    }
    let out = cmd
        .args(["list", "--id", &ids.join(","), "--status", "closed", "--limit", "0", "--json"])
        .output()
        .map_err(|e| format!("running bd list: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{text}{}", String::from_utf8_lossy(&out.stderr)));
    }
    let start = text.find(['[', '{']).ok_or("bd list: no JSON")?;
    let v: serde_json::Value = serde_json::from_str(&text[start..]).map_err(|e| format!("bd list --json: {e}"))?;
    let rows = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let field = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(rows.iter().filter(|r| field(r, "status") == "closed").map(|r| (field(r, "id"), field(r, "close_reason"))).filter(|(id, _)| !id.is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_labels_and_a_supersedes_dependency() {
        let text = r#"[{
            "id": "sp-1",
            "status": "closed",
            "labels": ["spira-dropped", "repo:service"],
            "dependencies": [
                {"id": "sp-2", "dependency_type": "supersedes"},
                {"id": "sp-3", "dependency_type": "blocks"}
            ]
        }]"#;
        let rec = parse(text, "sp-1").unwrap();
        assert_eq!(rec.status, BdStatus::Closed);
        assert!(rec.labels.contains("spira-dropped"));
        assert_eq!(rec.supersedes.as_deref(), Some("sp-2"));
    }

    #[test]
    fn a_bead_with_no_labels_or_dependencies_parses_cleanly() {
        let text = r#"[{"id": "sp-1", "status": "open"}]"#;
        let rec = parse(text, "sp-1").unwrap();
        assert_eq!(rec.status, BdStatus::Open);
        assert!(rec.labels.is_empty());
        assert!(rec.supersedes.is_none());
    }

    #[test]
    fn an_error_response_is_refused_not_defaulted_to_open() {
        let text = r#"{"error": "no issues found matching the provided IDs"}"#;
        assert!(parse(text, "sp-missing").is_err());
    }

    #[test]
    fn a_non_supersedes_dependency_is_not_mistaken_for_one() {
        let text = r#"[{"id": "sp-1", "status": "open", "dependencies": [{"id": "sp-2", "dependency_type": "blocks"}]}]"#;
        let rec = parse(text, "sp-1").unwrap();
        assert!(rec.supersedes.is_none());
    }
}
