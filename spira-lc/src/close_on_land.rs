//! `close-on-land <id> [sha]` — the only place a work bead is closed for a landed reason.
//! The landing itself is recorded first, on the lifecycle record (a `ContentOnBase` event
//! whose proof is `landed:<sha>`) — the one record; nothing here writes a second ledger.
//! The bd close that follows acts only while the bead is `open`/`in_progress` and carries
//! the submitted label: one already closed, or never submitted, is left alone.
//! Best-effort throughout and always exits 0, since every caller discards the answer.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Every subprocess here runs under `timeout` (call-deadline). A close or reap cut short is
/// the same miss as one that failed: CHECK 5 and the Sending's sweep stay the backstops.
const CALL_SECS: &str = "5";

pub struct Row {
    pub raw_status: String,
    pub labels: Vec<String>,
}

pub fn parse_row(text: &str) -> Option<Row> {
    let start = text.find(['[', '{'])?;
    let v: Value = serde_json::from_str(&text[start..]).ok()?;
    let item = v.as_array().map(|a| a.first().cloned().unwrap_or(Value::Null)).unwrap_or(v);
    let raw_status = item.get("status")?.as_str()?.to_string();
    let labels = item.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    Some(Row { raw_status, labels })
}

pub fn should_close(row: &Row, submitted_label: &str) -> bool {
    row.raw_status != "closed" && row.labels.iter().any(|l| l == submitted_label)
}

fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

/// The `ContentOnBase` proof this verb records: the land ref's sha the caller saw the work at.
pub fn proof(sha: &str) -> String {
    format!("landed:{}", if sha.is_empty() { "unknown" } else { sha })
}

pub fn reason(sha: &str) -> String {
    let shown = if sha.is_empty() { "unknown" } else { sha };
    format!("OUTCOME: landed\nClosed by the landing pass: work landed at {shown} (law-closed-is-not-landed).\n")
}

fn bdq_close(id: &str, reason: &str) -> bool {
    let Ok(mut child) = Command::new("timeout").args([CALL_SECS, "bdq", "close", id, "--reason-file", "-"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(reason.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn show_row(id: &str) -> Option<Row> {
    let db = spira_config::resolve::key_for_process("SPIRA_DB").ok().filter(|d| !d.is_empty())?;
    let bd = spira_config::resolve::key_for_process("SPIRA_BD").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "bd".into());
    let out = Command::new("timeout").arg(CALL_SECS).arg(bd).args(["-C", &db, "show", id, "--json"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| parse_row(&String::from_utf8_lossy(&out.stdout))).flatten()
}

fn branch_exists(root: &str, branch: &str) -> bool {
    Command::new("timeout").args([CALL_SECS, "git", "-C", root, "show-ref", "--verify", "-q", &format!("refs/heads/{branch}")]).status().map(|s| s.success()).unwrap_or(false)
}

fn reap(id: &str, branch: &str, root: &str, why: &str) -> Result<(), String> {
    let mut c = Command::new("timeout");
    c.args([CALL_SECS, "sending", "reap-landed-branch"]);
    if let Ok(f) = std::env::var("SPIRA_STATUS_FILE") {
        c.arg("--status-from").arg(f);
    }
    c.args([id, branch, root, why]).stdin(Stdio::null()).stderr(Stdio::null());
    match c.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Err(if t.is_empty() { "unknown".into() } else { t })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Record the landing on the lifecycle record. A refusal (already terminal, an ask hold) or an
/// unreachable record is reported, never fatal: the bd close and the reap do not depend on it.
pub fn record_landing(id: &str, sha: &str, record: &mut dyn FnMut(&str, &str) -> i32) -> i32 {
    let rc = record(id, &proof(sha));
    if rc != 0 {
        println!("land-close {id}: lifecycle content-on-base {} not applied (rc={rc})", proof(sha));
    }
    rc
}

/// `record(id, proof)` applies the `ContentOnBase` event on the lifecycle record (main.rs
/// hands in the caller-verb path, so the switch and the machine are read exactly as
/// `spira-lc content-on-base` reads them) and returns its exit code.
pub fn run(args: &[String], record: &mut dyn FnMut(&str, &str) -> i32) -> i32 {
    let Some(id) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("spira-lc close-on-land: usage: close-on-land <bead-id> [sha]");
        return 2;
    };
    let sha = args.get(1).map(String::as_str).unwrap_or("");
    record_landing(id, sha, record);
    let label = spira_config::resolve::key_for_process("SPIRA_SUBMITTED_LABEL").ok().filter(|l| !l.is_empty()).unwrap_or_else(|| "spira-submitted".into());
    let Some(row) = show_row(id) else { return 0 };
    if !should_close(&row, &label) {
        return 0;
    }
    let shown = if sha.is_empty() { "unknown" } else { sha };
    if !bdq_close(id, &reason(sha)) {
        println!("land-close {id}: bd close failed — left submitted (LANDED is on the lifecycle record)");
        return 0;
    }
    println!("land-close {id}: closed at {shown} (submitted -> landed)");

    let (Some(repo_label), Some(branch)) = (label_value(&row.labels, "repo:"), label_value(&row.labels, "branch:")) else { return 0 };
    let Ok(home) = spira_config::resolve::locate_home_for_process() else { return 0 };
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let Some(root) = spira_config::repos::Registry::from_env(env, Path::new(&home)).root(repo_label) else { return 0 };
    if !branch_exists(&root, branch) {
        return 0;
    }
    match reap(id, branch, &root, &format!("landed at {shown}")) {
        Ok(()) => println!("land-close {id}: reaped branch {branch}"),
        Err(e) => println!("land-close {id}: branch {branch} not reaped: {e} — left for the Sending"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, labels: &[&str]) -> Row {
        Row { raw_status: status.into(), labels: labels.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn only_an_unclosed_submitted_bead_is_closed() {
        assert!(should_close(&row("open", &["spira-submitted"]), "spira-submitted"));
        assert!(should_close(&row("in_progress", &["spira-submitted"]), "spira-submitted"));
        assert!(!should_close(&row("closed", &["spira-submitted"]), "spira-submitted"));
        assert!(!should_close(&row("open", &["other"]), "spira-submitted"));
        assert!(!should_close(&row("open", &["spira-submitted"]), "custom-label"));
    }

    #[test]
    fn parses_bd_json_after_a_warning_line() {
        let r = parse_row("warning: x\n[{\"status\":\"open\",\"labels\":[\"a\",\"repo:r\"]}]").unwrap();
        assert_eq!(r.raw_status, "open");
        assert_eq!(r.labels, vec!["a", "repo:r"]);
        assert!(parse_row("no json").is_none());
    }

    #[test]
    fn the_landing_is_recorded_on_the_lifecycle_record_before_anything_else() {
        let mut seen: Vec<(String, String)> = Vec::new();
        let rc = record_landing("sp-x", "abc", &mut |id, p| {
            seen.push((id.to_string(), p.to_string()));
            0
        });
        assert_eq!(rc, 0);
        assert_eq!(record_landing("sp-x", "abc", &mut |_, _| 3), 3, "a refusal is reported back, not swallowed");
        // run() records before it reads bd at all, so a bead bd cannot show still lands.
        let src = include_str!("close_on_land.rs");
        let body = &src[src.find("pub fn run(").unwrap()..];
        assert!(body.find("record_landing(").unwrap() < body.find("show_row(").unwrap());
        assert_eq!(seen, vec![("sp-x".to_string(), "landed:abc".to_string())]);
        assert_eq!(proof(""), "landed:unknown");
    }

    #[test]
    fn the_retired_landstate_writer_is_not_called() {
        // Guard (sp-2c1n0): the ledger and `landing-pass mark` are deleted; this verb records
        // LANDED through the lifecycle record alone.
        let src = include_str!("close_on_land.rs");
        let needle = ["landing-pass", "\"mark\""].join("\", ");
        assert!(!src.contains(&needle), "close-on-land shells to the retired landstate writer again");
    }

    #[test]
    fn reason_names_the_sha_or_unknown() {
        assert!(reason("abc").contains("landed at abc"));
        assert!(reason("").contains("landed at unknown"));
    }
}
