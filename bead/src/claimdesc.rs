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

/// A bead an aeon holds: the lifecycle row WORKING, with a holder and a lease that has not
/// provably lapsed (sp-mve9i: the claim is the machine's row, never bd's `in_progress` and
/// `assignee`). A row with no lease counts as live: refusing wrongly costs a flag, missing an
/// edit costs a wasted session.
pub fn live_claim(row: Option<&spira_config::lc_state::Row>, now_epoch: i64) -> Option<LiveClaim> {
    let row = row.filter(|r| r.working())?;
    let holder = row.holder.as_deref().map(str::trim).filter(|h| !h.is_empty())?.to_string();
    if row.lease_until.is_some_and(|e| e <= now_epoch) {
        return None;
    }
    let lease = row.lease_until.map(iso_of).unwrap_or_else(|| "unknown".to_string());
    Some(LiveClaim { assignee: holder, lease })
}

/// `YYYY-MM-DDTHH:MM:SSZ` for an epoch second (the inverse of [`parse_iso`]'s UTC case).
pub fn iso_of(epoch: i64) -> String {
    let (days, secs) = (epoch.div_euclid(86_400), epoch.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
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
///
/// `run`/`mail` are `SPIRA_RUN`/`SPIRA_MAIL` (both registered config keys) — this is pure
/// logic, so it takes them as arguments rather than reading config itself (per Ryan
/// 2026-10-05: one source of config); callers resolve them through
/// `spira_config::process::cfg` at their own top level.
pub fn notify_live_aeon(id: &str, message: &str, run: &str, mail: &str) {
    if id.is_empty() || message.is_empty() || run.is_empty() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(run) else { return };
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
    let Ok(mut child) = Command::new("timeout")
        .args(["5", "mail", "send", aeon_id, "--from", "amend <amend@spira>", "--subject", "Update while you work"])
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

    /// sp-mve9i: the claim is the lifecycle row's — WORKING, its holder, its lease — and bd's
    /// status and assignee play no part.
    #[test]
    fn live_claim_is_the_lifecycle_rows_working_holder_and_lease() {
        use spira_config::lc_state::Row;
        let now = parse_iso("2026-10-02T12:00:00Z").unwrap();
        let row = |st: &str, holder: &str, lease: Option<i64>| Row {
            bead_id: "x".into(),
            state: st.into(),
            holder: Some(holder.to_string()).filter(|h| !h.is_empty()),
            lease_until: lease,
            holds: vec![],
        };
        let live = live_claim(Some(&row("WORKING", "aeon-x", Some(now + 3600))), now).unwrap();
        assert_eq!((live.assignee.as_str(), live.lease.as_str()), ("aeon-x", "2026-10-02T13:00:00Z"));
        assert!(live_claim(Some(&row("WORKING", "aeon-x", Some(now - 3600))), now).is_none());
        assert!(live_claim(Some(&row("READY", "aeon-x", Some(now + 3600))), now).is_none());
        assert!(live_claim(Some(&row("WORKING", "", Some(now + 3600))), now).is_none());
        assert_eq!(live_claim(Some(&row("WORKING", "aeon-x", None)), now).unwrap().lease, "unknown");
        assert!(live_claim(None, now).is_none());
    }

    #[test]
    fn iso_parses_offsets_and_fractions() {
        assert_eq!(iso_of(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(parse_iso(&iso_of(1_790_000_123)), Some(1_790_000_123));
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_iso("1970-01-01T00:00:01.250Z"), Some(1));
        assert_eq!(parse_iso("2000-03-01 00:00:00"), Some(951_868_800));
    }
}
