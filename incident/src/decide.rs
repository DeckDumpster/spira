//! Pure decision logic for the intake (DESIGN.md). Nothing here touches a process, a
//! clock, a file or the network — every fact a function needs is a parameter, so every
//! one of them is a table-driven unit test with no fixture database. The IO layer
//! (`real.rs`) is the only place that calls out to `bd`, `mail.sh` or the filesystem.

use sha2::{Digest, Sha256};

/// Deterministic 8-char hex label token for an external_ref string (incident.sh's
/// `_ref_hash`). sha256, first 8 hex chars: 32 bits, negligible collision risk across a
/// queue of hundreds of open beads, and a collision costs one extra list iteration rather
/// than a silent wrong answer (the dedup path still confirms external_ref itself).
pub fn ref_hash(reference: &str) -> String {
    let digest = Sha256::digest(reference.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}

/// incident.sh sanitises SPIRA_INCIDENT_CAUSE with
/// `tr -c 'a-zA-Z0-9-' '-' | sed 's/-\{2,\}/-/g;s/^-//;s/-$//'`, defaulting to "unrecorded"
/// when that collapses to nothing. A space in a label would otherwise split it into two
/// labels and desynchronise the recurrence-cause ladder.
pub fn sanitize_cause(raw: &str) -> String {
    let translated: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let mut collapsed = String::with_capacity(translated.len());
    let mut last_dash = false;
    for c in translated.chars() {
        if c == '-' {
            if !last_dash {
                collapsed.push(c);
            }
            last_dash = true;
        } else {
            collapsed.push(c);
            last_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches('-');
    if trimmed.is_empty() {
        "unrecorded".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `_reopen_cause`: a bead closed less than `interval_s` before the same fingerprint
/// recurred was still live — Ops had not yet had the interval's worth of time to see
/// whether the close held. Past the interval, the same event is an ordinary recurrence.
pub fn reopen_cause(closed_epoch: i64, now_epoch: i64, interval_s: i64) -> &'static str {
    if now_epoch - closed_epoch < interval_s {
        "closed-while-live"
    } else {
        "recurrence"
    }
}

/// A bead's status as `_dedup_incident` needs it: whether it counts as open-ish
/// (open/in_progress) or closed, plus the fields the decision depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeadRow {
    pub id: String,
    pub status: BeadStatus,
    pub external_ref: Option<String>,
    pub labels: Vec<String>,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeadStatus {
    Open,
    InProgress,
    Closed,
    Other,
}

/// Outcome of `_dedup_incident`/`incident-dedup-decision.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupHit {
    Open { id: String },
    Closed { id: String, closed_at: Option<String> },
}

/// One pass of the scan incident-dedup-decision.py performed: `open`=true selects
/// open/in_progress candidates, `open`=false selects closed ones. `skip_ref_labeled`=true
/// is the O(N) fallback pass — it must skip any bead already carrying a `ref:` label,
/// because those were already checked on the label-keyed pass and re-matching them here
/// would let an unrelated bead's unlabeled duplicate short-circuit on the wrong row.
pub fn dedup_scan(rows: &[BeadRow], open: bool, skip_ref_labeled: bool, reference: &str) -> Option<DedupHit> {
    for row in rows {
        if skip_ref_labeled && row.labels.iter().any(|l| l.starts_with("ref:")) {
            continue;
        }
        if row.external_ref.as_deref() != Some(reference) {
            continue;
        }
        if open {
            if matches!(row.status, BeadStatus::Open | BeadStatus::InProgress) {
                return Some(DedupHit::Open { id: row.id.clone() });
            }
        } else if matches!(row.status, BeadStatus::Closed) {
            return Some(DedupHit::Closed { id: row.id.clone(), closed_at: row.closed_at.clone() });
        }
    }
    None
}

/// Prints the first 2000 bytes of the payload when it differs from the last recorded
/// hash (or the bead was just reopened); prints nothing when unchanged — an unchanged
/// base-suite red must not grow the bead body every recurrence (sp-obwc, `_recur_note_body`).
/// Returns (note_suffix, this payload's hash label).
pub fn recur_note_body(prev_hash_label: Option<&str>, payload: &[u8], was_closed: bool) -> (Option<String>, String) {
    let digest = Sha256::digest(payload);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let cur = hex[..16].to_string();
    let prev_label = format!("payload-hash:{cur}");
    if !was_closed && prev_hash_label == Some(prev_label.as_str()) {
        (None, cur)
    } else {
        let truncated: Vec<u8> = payload.iter().take(2000).copied().collect();
        let body = String::from_utf8_lossy(&truncated).into_owned();
        (Some(format!("\n{body}")), cur)
    }
}

/// Whether a filing at recurrence count `n` crosses the Sin threshold and should escalate:
/// the events query succeeded (not blind), not exempt, at or past `sin_at`, and not
/// already labelled `sin`.
pub fn crosses_sin(n: u32, sin_at: u32, sin_exempt: bool, events_unknown: bool, already_sin: bool) -> bool {
    !events_unknown && !sin_exempt && n >= sin_at && !already_sin
}

/// `spira_home_repo`: `$SPIRA_HOME_REPO`, else the basename of `$SPIRA_REPO` (falling back
/// to `$SPIRA_HOME`). All three passed in explicitly rather than read from `std::env` here,
/// so the whole decision is a table-driven unit test.
pub fn home_repo(home_repo_env: Option<&str>, repo_env: Option<&str>, home_env: Option<&str>) -> String {
    if let Some(h) = home_repo_env {
        if !h.is_empty() {
            return h.to_string();
        }
    }
    let base = repo_env.filter(|s| !s.is_empty()).or(home_env).unwrap_or("");
    std::path::Path::new(base).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
}

/// `repo_names`: every repo-map row's name column, skipping `#`-comments and any row with
/// fewer than 2 `|`-separated fields (lib.sh's `awk ... NF > 1`).
pub fn repo_names(repo_map_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in repo_map_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() <= 1 {
            continue;
        }
        let name = parts[0].trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// `_provenance`: `"<unit> on <host>: <path>"`.
pub fn provenance(unit: &str, host: &str, path: &str) -> String {
    format!("{unit} on {host}: {path}")
}

/// `_unit_from_cgroup`'s string half: the last path component of a cgroup line, kept only
/// when it looks like a systemd unit. `?` (never a guess) otherwise — the cgroup line
/// itself is read in `real.rs`.
pub fn unit_from_cgroup_line(line: &str) -> String {
    let last = line.rsplit('/').next().unwrap_or("");
    if last.ends_with(".service") || last.ends_with(".timer") || last.ends_with(".scope") {
        last.to_string()
    } else {
        "?".to_string()
    }
}

/// `_bdq_check_repo_label`: a `repo:<name>` in the create labels must be the home repo or
/// a name the repo-map carries. Returns the invalid name to report, or None (allowed).
///
/// PORTED, NOT DROPPED. `incident.sh` never calls `bd` directly for `create` — every path
/// goes through lib.sh's `bdq`, which applies this fence (plus the two below) underneath.
/// This crate's `real.rs` shells to `bd` directly, so calling `bd create` without
/// reimplementing these three checks here would be a real regression, not a
/// simplification: an invalid `repo:` label, a harness-halting phrase, or a
/// schema_migrations DELETE that `bdq` refuses today would go through silently. They are
/// small, self-contained and change rarely, so porting them costs little and keeps parity;
/// lib.sh's own rewrite wave can delete these three functions here once bdq's fences move
/// to a shared crate this one can depend on instead.
pub fn invalid_repo_label(labels: &str, home_repo: &str, known_repos: &[String]) -> Option<String> {
    let repo_val = labels
        .split(',')
        .find_map(|l| l.strip_prefix("repo:"))
        .map(str::to_string)?;
    if repo_val == home_repo || known_repos.iter().any(|r| r == &repo_val) {
        None
    } else {
        Some(repo_val)
    }
}

/// `_bdq_check_destructive`: title+description containing one of a fixed set of
/// harness-halting phrases is refused unless `needs-ryan` (or whatever `ask_label` is
/// configured to) is already on the bead — the label that records the danger was
/// acknowledged at filing time (sp-6hdi). Case-insensitive. Returns the matched phrase.
pub fn destructive_phrase(title: &str, description: &str, labels: &str, ask_label: &str) -> Option<String> {
    if labels.split(',').any(|l| l == ask_label) {
        return None;
    }
    let text = format!("{title} {description}").to_lowercase();
    const PATTERNS: &[&str] = &[
        "world.sh stop",
        "spira-world down",
        "systemd/install.sh",
        "daemon-reload",
        "schema migrat",
        "world stopped",
    ];
    for p in PATTERNS {
        if text.contains(p) {
            return Some((*p).to_string());
        }
    }
    // "systemctl (stop|restart) spira-" — the one pattern with an alternation.
    for verb in ["stop", "restart"] {
        let needle = format!("systemctl {verb} spira-");
        if text.contains(&needle) {
            return Some(needle);
        }
    }
    None
}

/// `_bdq_check_schema_delete`: refused regardless of `needs-ryan` — approved three times
/// while still wrong (sp-1khst).
pub fn contains_schema_delete(title: &str, description: &str) -> bool {
    let text = format!("{title} {description}").to_lowercase();
    let words: Vec<&str> = text.split_whitespace().collect();
    for i in 0..words.len() {
        if words[i] == "delete" && words.get(i + 1) == Some(&"from") && words.get(i + 2) == Some(&"schema_migrations")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_hash_is_deterministic_and_8_hex_chars() {
        let h1 = ref_hash("incident:foo");
        let h2 = ref_hash("incident:foo");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 8);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(h1, ref_hash("incident:bar"));
    }

    #[test]
    fn sanitize_cause_collapses_and_trims_and_defaults() {
        assert_eq!(sanitize_cause("suite-red"), "suite-red");
        assert_eq!(sanitize_cause("suite red!!"), "suite-red");
        assert_eq!(sanitize_cause("  --leading"), "leading");
        assert_eq!(sanitize_cause("trailing--  "), "trailing");
        assert_eq!(sanitize_cause(""), "unrecorded");
        assert_eq!(sanitize_cause("   "), "unrecorded");
        assert_eq!(sanitize_cause("---"), "unrecorded");
    }

    #[test]
    fn reopen_cause_classifies_by_interval() {
        assert_eq!(reopen_cause(1000, 1100, 1800), "closed-while-live");
        assert_eq!(reopen_cause(1000, 3000, 1800), "recurrence");
        // Exactly at the boundary is NOT still-live (bash: `-lt`, strictly less than).
        assert_eq!(reopen_cause(1000, 2800, 1800), "recurrence");
    }

    fn row(id: &str, status: BeadStatus, ext_ref: Option<&str>, labels: &[&str], closed_at: Option<&str>) -> BeadRow {
        BeadRow {
            id: id.into(),
            status,
            external_ref: ext_ref.map(str::to_string),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            closed_at: closed_at.map(str::to_string),
        }
    }

    #[test]
    fn dedup_scan_label_keyed_pass_finds_open_bead() {
        let rows = vec![row("sp-a", BeadStatus::Open, Some("incident:x"), &["ref:aa"], None)];
        assert_eq!(dedup_scan(&rows, true, false, "incident:x"), Some(DedupHit::Open { id: "sp-a".into() }));
    }

    #[test]
    fn dedup_scan_fallback_skips_ref_labeled_rows() {
        // A bead that already carries a ref: label was checked on the label-keyed pass;
        // the fallback must not re-match it even though the external_ref matches too.
        let rows = vec![row("sp-a", BeadStatus::Open, Some("incident:x"), &["ref:aa"], None)];
        assert_eq!(dedup_scan(&rows, true, true, "incident:x"), None);
    }

    #[test]
    fn dedup_scan_fallback_finds_unlabeled_bead() {
        let rows = vec![row("sp-a", BeadStatus::Open, Some("incident:x"), &[], None)];
        assert_eq!(dedup_scan(&rows, true, true, "incident:x"), Some(DedupHit::Open { id: "sp-a".into() }));
    }

    #[test]
    fn dedup_scan_closed_pass_returns_closed_at() {
        let rows = vec![row("sp-a", BeadStatus::Closed, Some("incident:x"), &[], Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            dedup_scan(&rows, false, false, "incident:x"),
            Some(DedupHit::Closed { id: "sp-a".into(), closed_at: Some("2026-09-01T00:00:00Z".into()) })
        );
    }

    #[test]
    fn dedup_scan_no_match_returns_none() {
        let rows = vec![row("sp-a", BeadStatus::Open, Some("incident:y"), &[], None)];
        assert_eq!(dedup_scan(&rows, true, false, "incident:x"), None);
    }

    #[test]
    fn dedup_scan_wrong_status_bucket_returns_none() {
        // An in_progress bead is not found by the closed-status scan.
        let rows = vec![row("sp-a", BeadStatus::InProgress, Some("incident:x"), &[], None)];
        assert_eq!(dedup_scan(&rows, false, false, "incident:x"), None);
    }

    #[test]
    fn recur_note_body_empty_when_payload_unchanged_and_not_reopened() {
        let payload = b"same payload";
        let (_, hash) = recur_note_body(None, payload, false);
        let prev = format!("payload-hash:{hash}");
        let (note, hash2) = recur_note_body(Some(&prev), payload, false);
        assert_eq!(note, None);
        assert_eq!(hash, hash2);
    }

    #[test]
    fn recur_note_body_present_when_payload_changed() {
        let (note, _) = recur_note_body(Some("payload-hash:deadbeef"), b"new payload", false);
        assert!(note.unwrap().contains("new payload"));
    }

    #[test]
    fn recur_note_body_present_on_reopen_even_if_unchanged() {
        let payload = b"same payload";
        let (_, hash) = recur_note_body(None, payload, false);
        let prev = format!("payload-hash:{hash}");
        let (note, _) = recur_note_body(Some(&prev), payload, true);
        assert!(note.is_some(), "a reopen must carry the note even with an unchanged payload");
    }

    #[test]
    fn recur_note_body_truncates_to_2000_bytes() {
        let payload = vec![b'x'; 5000];
        let (note, _) = recur_note_body(None, &payload, false);
        // 1 leading newline + 2000 'x'.
        assert_eq!(note.unwrap().len(), 2001);
    }

    #[test]
    fn sin_thresholds() {
        assert!(crosses_sin(5, 5, false, false, false));
        assert!(!crosses_sin(4, 5, false, false, false), "below threshold");
        assert!(!crosses_sin(5, 5, true, false, false), "exempt");
        assert!(!crosses_sin(5, 5, false, true, false), "blind on unknown count");
        assert!(!crosses_sin(5, 5, false, false, true), "already labelled sin — no second page");
    }

    #[test]
    fn home_repo_prefers_explicit_env() {
        assert_eq!(home_repo(Some("brain"), Some("/whatever"), None), "brain");
    }

    #[test]
    fn home_repo_falls_back_to_repo_basename() {
        assert_eq!(home_repo(None, Some("/home/ryan/spira/harness"), None), "harness");
    }

    #[test]
    fn home_repo_falls_back_to_home_when_no_repo() {
        assert_eq!(home_repo(None, None, Some("/home/ryan/spira/harness")), "harness");
    }

    #[test]
    fn repo_names_skips_comments_and_short_rows() {
        let text = "# comment\nbrain | /path/to/brain | push\nlonewolf\nspira | /path/to/spira | hold\n";
        assert_eq!(repo_names(text), vec!["brain".to_string(), "spira".to_string()]);
    }

    #[test]
    fn provenance_format() {
        assert_eq!(provenance("spira-groom.service", "box1", "/some/path"), "spira-groom.service on box1: /some/path");
    }

    #[test]
    fn unit_from_cgroup_line_recognises_unit_suffixes() {
        assert_eq!(unit_from_cgroup_line("0::/user.slice/spira-groom.service"), "spira-groom.service");
        assert_eq!(unit_from_cgroup_line("0::/user.slice/spira-maechen.timer"), "spira-maechen.timer");
        assert_eq!(unit_from_cgroup_line("0::/user.slice/session.scope"), "session.scope");
        assert_eq!(unit_from_cgroup_line("0::/user.slice/something-else"), "?");
        assert_eq!(unit_from_cgroup_line(""), "?");
    }

    #[test]
    fn invalid_repo_label_allows_home_repo_and_known_repos() {
        let known = vec!["spira".to_string(), "brain".to_string()];
        assert_eq!(invalid_repo_label("plan,repo:brain", "spira", &known), None);
        assert_eq!(invalid_repo_label("plan,repo:spira", "spira", &known), None);
        assert_eq!(invalid_repo_label("plan", "spira", &known), None, "no repo: label at all is allowed");
    }

    #[test]
    fn invalid_repo_label_refuses_unknown_repo() {
        let known = vec!["spira".to_string()];
        assert_eq!(invalid_repo_label("plan,repo:nope", "spira", &known), Some("nope".to_string()));
    }

    #[test]
    fn destructive_phrase_matches_and_is_bypassed_by_ask_label() {
        assert_eq!(destructive_phrase("needs the world stopped", "", "plan", "needs-ryan"), Some("world stopped".to_string()));
        assert_eq!(destructive_phrase("needs the world stopped", "", "plan,needs-ryan", "needs-ryan"), None);
        assert_eq!(
            destructive_phrase("run systemctl restart spira-gate", "", "plan", "needs-ryan"),
            Some("systemctl restart spira-".to_string())
        );
        assert_eq!(destructive_phrase("ordinary title", "ordinary body", "plan", "needs-ryan"), None);
    }

    #[test]
    fn schema_delete_is_detected_regardless_of_case_or_spacing() {
        assert!(contains_schema_delete("DELETE   FROM schema_migrations", ""));
        assert!(contains_schema_delete("", "please delete from schema_migrations now"));
        assert!(!contains_schema_delete("delete from somewhere_else", ""));
    }
}
