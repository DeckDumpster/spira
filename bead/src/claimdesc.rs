//! An edit to a claimed bead's description is invisible to the builder working it — it reads the
//! bead once, at claim. `bdq update` refuses a title/description edit on a live claim unless
//! `--force-claimed "<reason>"` acknowledges it; the aeon compares `claim_desc_hash` against the
//! description at close and reopens a close where they differ.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

pub const FORCE_FLAG: &str = "--force-claimed";
pub const HASH_KEY: &str = "claim_desc_hash";

#[derive(Debug, PartialEq)]
pub struct LiveClaim {
    pub assignee: String,
    pub lease: String,
}

#[derive(Debug, PartialEq)]
pub struct UpdateArgs {
    pub passthrough: Vec<String>,
    pub force: Option<String>,
    pub touches_description: bool,
    pub id: Option<String>,
}

fn first_row(show_json: &str) -> Option<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(show_json.trim()).ok()? {
        serde_json::Value::Array(mut a) if !a.is_empty() => Some(a.remove(0)),
        o @ serde_json::Value::Object(_) => Some(o),
        _ => None,
    }
}

/// sha256 hex of the description in a `bd show --json` blob; `None` when unreadable.
pub fn desc_hash(show_json: &str) -> Option<String> {
    let row = first_row(show_json)?;
    let desc = row.get("description").and_then(|d| d.as_str()).unwrap_or("");
    Some(Sha256::digest(desc.as_bytes()).iter().map(|b| format!("{b:02x}")).collect())
}

pub fn metadata_value(show_json: &str, key: &str) -> String {
    first_row(show_json)
        .and_then(|r| r.get("metadata").and_then(|m| m.get(key)).and_then(|v| v.as_str().map(String::from)))
        .unwrap_or_default()
}

/// Splits spira's own `--force-claimed <reason>` pair out of `update`'s args (bd has no such
/// flag) and reports whether the call edits the title or description.
pub fn parse_update_args(args: &[String]) -> UpdateArgs {
    let touches_description =
        args.iter().any(|a| matches!(a.as_str(), "--title" | "-d" | "--description" | "--body-file" | "--stdin"));
    let id = args.iter().take_while(|a| !a.starts_with('-')).next().cloned();
    let mut passthrough = Vec::new();
    let mut force = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == FORCE_FLAG {
            force = Some(it.next().cloned().unwrap_or_default());
        } else {
            passthrough.push(a.clone());
        }
    }
    UpdateArgs { passthrough, force, touches_description, id }
}

/// An in_progress bead with an assignee and a lease that has not provably lapsed. An
/// unparseable lease counts as live: refusing wrongly costs a flag, missing an edit costs a
/// wasted session.
pub fn live_claim(show_json: &str, now_epoch: i64) -> Option<LiveClaim> {
    let row = first_row(show_json)?;
    if row.get("status").and_then(|s| s.as_str()) != Some("in_progress") {
        return None;
    }
    let assignee = row.get("assignee").and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
    if assignee.is_empty() {
        return None;
    }
    let lease = row.get("lease_expires_at").and_then(|l| l.as_str()).filter(|l| !l.is_empty());
    if let Some(l) = lease {
        if parse_iso(l).is_some_and(|e| e <= now_epoch) {
            return None;
        }
    }
    Some(LiveClaim { assignee, lease: lease.unwrap_or("unknown").to_string() })
}

pub fn refusal(id: &str, c: &LiveClaim) -> String {
    format!(
        "bead: {id} is claimed by {} (lease expires {}) — editing its title/description now is invisible to the builder already working from what it read at claim. Pass {FORCE_FLAG} \"<reason>\" to override.\n",
        c.assignee, c.lease
    )
}

pub fn override_note(actor: &str, c: &LiveClaim, reason: &str) -> String {
    let reason = if reason.is_empty() { "no reason given" } else { reason };
    format!(
        "Title/description edited by {actor} while claimed by {} (lease expires {}), overridden with {FORCE_FLAG}: {reason}",
        c.assignee, c.lease
    )
}

pub fn holder_message(actor: &str, reason: &str) -> String {
    let reason = if reason.is_empty() { "no reason given" } else { reason };
    format!("Its title or description was just edited while you hold the claim (overridden by {actor}: {reason}).")
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// RFC3339 / `YYYY-MM-DD HH:MM:SS` with optional fraction and `Z`/`±hh:mm` offset.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let epoch = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?) * 86_400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?;
    let mut rest = s.get(19..)?;
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let off = match rest.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
            (hh * 3600 + mm * 60) * if sign == '+' { 1 } else { -1 }
        }
        _ => 0,
    };
    Some(epoch - off)
}

/// Mails the aeon actually working `id`, if one is alive and its mailbox is open.
pub fn notify_live_aeon(id: &str, message: &str) {
    if id.is_empty() || message.is_empty() {
        return;
    }
    let Some(run) = std::env::var("SPIRA_RUN").ok().filter(|r| !r.is_empty()) else { return };
    let Ok(entries) = std::fs::read_dir(&run) else { return };
    let suffix = format!("-{id}.pid");
    let mut candidates: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("aeon-") && n.ends_with(&suffix))
        .collect();
    candidates.sort();
    for name in candidates {
        if !aeon_alive(&format!("{run}/{name}")) {
            continue;
        }
        let mail = std::env::var("SPIRA_MAIL").unwrap_or_default();
        if !Path::new(&format!("{mail}/aeon-{id}/new")).is_dir() {
            break;
        }
        mail_send(&format!("aeon-{id}"), message);
        break;
    }
}

pub fn aeon_alive(pidfile: &str) -> bool {
    strand::probe::aeon_alive(Path::new(pidfile))
}

fn mail_send(aeon_id: &str, body: &str) {
    let Ok(mut child) = Command::new("mail")
        .args(["send", aeon_id, "--from", "amend <amend@spira>", "--subject", "Update while you work"])
        .env("SPIRA_MAIL_LINT_CONSIDERED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn show(status: &str, assignee: &str, lease: &str) -> String {
        format!(r#"[{{"id":"x","status":"{status}","assignee":"{assignee}","lease_expires_at":"{lease}","description":"d"}}]"#)
    }

    #[test]
    fn hash_follows_the_description_only() {
        let h1 = desc_hash(r#"[{"description":"one"}]"#).unwrap();
        assert_eq!(h1, desc_hash(r#"{"description":"one","title":"t"}"#).unwrap());
        assert_ne!(h1, desc_hash(r#"[{"description":"two"}]"#).unwrap());
        assert_eq!(desc_hash("not json"), None);
        assert_eq!(desc_hash(r#"[{"id":"x"}]"#).unwrap(), desc_hash(r#"[{"description":""}]"#).unwrap());
    }

    #[test]
    fn metadata_reads_a_key_or_empty() {
        assert_eq!(metadata_value(r#"[{"metadata":{"claim_desc_hash":"abc"}}]"#, HASH_KEY), "abc");
        assert_eq!(metadata_value(r#"[{"id":"x"}]"#, HASH_KEY), "");
    }

    #[test]
    fn force_pair_is_stripped_and_description_edits_detected() {
        let u = parse_update_args(&a(&["sp-1", "--body-file", "f", "--force-claimed", "why", "--priority", "1"]));
        assert_eq!(u.passthrough, a(&["sp-1", "--body-file", "f", "--priority", "1"]));
        assert_eq!(u.force.as_deref(), Some("why"));
        assert!(u.touches_description);
        assert_eq!(u.id.as_deref(), Some("sp-1"));
        let u = parse_update_args(&a(&["sp-1", "--set-metadata", "k=v"]));
        assert!(!u.touches_description);
        assert_eq!(u.force, None);
    }

    #[test]
    fn live_claim_needs_in_progress_assignee_and_unlapsed_lease() {
        let now = parse_iso("2026-10-02T12:00:00Z").unwrap();
        let live = live_claim(&show("in_progress", "aeon-x", "2026-10-02T13:00:00Z"), now).unwrap();
        assert_eq!(live.assignee, "aeon-x");
        assert!(live_claim(&show("in_progress", "aeon-x", "2026-10-02T11:00:00Z"), now).is_none());
        assert!(live_claim(&show("open", "aeon-x", "2026-10-02T13:00:00Z"), now).is_none());
        assert!(live_claim(&show("in_progress", "", "2026-10-02T13:00:00Z"), now).is_none());
        assert!(live_claim(&show("in_progress", "aeon-x", "garbage"), now).is_some());
    }

    #[test]
    fn iso_parses_offsets_and_fractions() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_iso("1970-01-01T00:00:01.250Z"), Some(1));
        assert_eq!(parse_iso("2000-03-01 00:00:00"), Some(951_868_800));
    }
}
