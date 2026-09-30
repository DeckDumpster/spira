//! Pure decision logic for the Maechen trigger (DESIGN.md "Contract"). Nothing here touches
//! a filesystem, a clock or a subprocess — every function is a total function of its
//! arguments, which is what makes it unit-testable without a database or a git checkout.

use std::collections::HashSet;

fn is_id_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// Leftmost match of `[a-z0-9]+-[a-z0-9]+` anywhere in `s` (bash's `awk match()` semantics
/// for the "spira: land " and "spira/" commit forms), or `None`.
fn first_bead_id(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if is_id_char(b[i]) {
            let start = i;
            let mut j = i;
            while j < n && is_id_char(b[j]) {
                j += 1;
            }
            if j < n && b[j] == b'-' {
                let id_start2 = j + 1;
                let mut k = id_start2;
                while k < n && is_id_char(b[k]) {
                    k += 1;
                }
                if k > id_start2 {
                    return Some(s[start..k].to_string());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// `[a-z0-9]+-[a-z0-9]+` matched from position 0 of `line`, immediately followed by `:`
/// (the aeon commit-prefix form `<id>: ...`), or `None`.
fn id_prefix_colon(line: &str) -> Option<String> {
    let b = line.as_bytes();
    let n = b.len();
    let mut j = 0;
    while j < n && is_id_char(b[j]) {
        j += 1;
    }
    if j == 0 || j >= n || b[j] != b'-' {
        return None;
    }
    let id_start2 = j + 1;
    let mut k = id_start2;
    while k < n && is_id_char(b[k]) {
        k += 1;
    }
    if k == id_start2 {
        return None;
    }
    if k < n && b[k] == b':' {
        Some(line[0..k].to_string())
    } else {
        None
    }
}

/// The bead id a single commit subject names as landed, by the three forms the harness
/// writes plus the aeon prefix form (maechen-trigger.sh `_count_landings`'s awk program).
/// Dispatch is mutually exclusive and ordered exactly as the awk program's pattern blocks:
/// `spira: land ` first, then any `spira/`, then the bare prefix form.
pub fn landing_id(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("spira: land ") {
        return first_bead_id(rest);
    }
    if let Some(pos) = line.find("spira/") {
        return first_bead_id(&line[pos + "spira/".len()..]);
    }
    id_prefix_colon(line)
}

/// Distinct bead ids landed in a `git log --format=%s` subject list, in first-seen order
/// (a merge and its landing commit for the same bead count once).
pub fn parse_landing_ids(subjects: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for line in subjects.lines() {
        if let Some(id) = landing_id(line) {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    ids
}

/// One line of `spira/repo-map`: `name | path | ...`. Comment (`#`) and blank names are
/// skipped, as the bash `while IFS='|' read` loop does; leading/trailing whitespace on name
/// and path is trimmed.
pub fn parse_repo_map(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '|');
        let name = parts.next().unwrap_or("").trim();
        let path = parts.next().unwrap_or("").trim();
        if name.is_empty() || name.starts_with('#') {
            continue;
        }
        if path.is_empty() {
            continue;
        }
        out.push((name.to_string(), path.to_string()));
    }
    out
}

/// Fires when `elapsed >= threshold` seconds have passed since the last completed pass.
pub fn time_trigger(elapsed_secs: i64, threshold_secs: i64) -> bool {
    elapsed_secs >= threshold_secs
}

/// Fires when the landing count since the watermark reaches the interval.
pub fn landing_trigger(count: u64, threshold: u64) -> bool {
    count >= threshold
}

/// `INVALID-CLOSED <id> ...` / `UNFILED-FOLLOW <id> ...` lines from `detect_invalid_closed`,
/// deduplicated by bead id (first occurrence wins), `ALLOWED-IC` rows excluded. Mirrors the
/// bash's substring-containment dedup (`case "$acc" in *"$_bid"*)`), which this replaces with
/// an exact id set — a deliberate, named difference (DESIGN.md Decisions): the two agree on
/// every real bead id, which is never a substring of another bead id's row.
pub fn invalid_closed_rows(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let matched = line.starts_with("INVALID-CLOSED ") || line.starts_with("UNFILED-FOLLOW ");
        if !matched {
            continue;
        }
        let rest = line.splitn(2, ' ').nth(1).unwrap_or("");
        let bid = rest.split_whitespace().next().unwrap_or("");
        if bid.is_empty() {
            continue;
        }
        if seen.insert(bid.to_string()) {
            rows.push(line.to_string());
        }
    }
    rows
}

/// The partition labels a trigger bead carries, so `maechen.fayth`'s `FAYTH_LABELS`
/// predicate — and the dedup query below — never disagree.
pub fn trigger_labels(scope_label: &str, maechen_label: &str) -> String {
    if scope_label.is_empty() {
        maechen_label.to_string()
    } else {
        format!("{scope_label},{maechen_label}")
    }
}

pub struct TriggerReason {
    pub time: bool,
    pub landing: bool,
    pub invalid_closed: bool,
}

impl TriggerReason {
    pub fn any(&self) -> bool {
        self.time || self.landing || self.invalid_closed
    }
}

/// The comma-joined reason clause for the bead title/description, in the order the bash
/// builds it: elapsed, then landings, then invalid-closed rows.
pub fn reason_text(r: &TriggerReason, elapsed: i64, landing_count: u64, invalid_closed_n: usize) -> String {
    let mut parts = Vec::new();
    if r.time {
        parts.push(format!("{elapsed}s elapsed"));
    }
    if r.landing {
        parts.push(format!("{landing_count} landings"));
    }
    if r.invalid_closed {
        parts.push(format!("{invalid_closed_n} invalid-closed row(s)"));
    }
    parts.join(", ")
}

/// The trigger bead's description, mirroring the bash's `_desc` assembly.
pub fn description(
    max_beads: u64,
    reason: &str,
    lastpass_ts: i64,
    watermark_ts: i64,
    invalid_closed_rows_text: Option<&str>,
) -> String {
    let mut d = format!(
        "Scheduled trigger: the Maechen persona will claim this bead, run a retrospective pass over the failure distribution, identify recurring failure classes, and cut at most {max_beads} remedy beads. Trigger: {reason} (lastpass ts={lastpass_ts}, watermark ts={watermark_ts}). See spira/chamber/maechen.md for the pass procedure."
    );
    if let Some(rows) = invalid_closed_rows_text {
        d.push_str("\n\nClosed-record rows (detect_invalid_closed output):\n");
        d.push_str(rows);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_id_spira_land_form() {
        assert_eq!(landing_id("spira: land sp-herv0"), Some("sp-herv0".to_string()));
    }

    #[test]
    fn landing_id_merge_branch_form() {
        assert_eq!(
            landing_id("Merge branch 'spira/sp-z61hj' into local/main"),
            Some("sp-z61hj".to_string())
        );
    }

    #[test]
    fn landing_id_merge_pr_form() {
        assert_eq!(
            landing_id("Merge pull request #42 from rgantt/spira/sp-ak7qm"),
            Some("sp-ak7qm".to_string())
        );
    }

    #[test]
    fn landing_id_aeon_prefix_form() {
        assert_eq!(
            landing_id("sp-htrqk: retire test-batch-red-main.sh — subject covered by queue crate unit tests"),
            Some("sp-htrqk".to_string())
        );
    }

    #[test]
    fn landing_id_prefix_form_requires_leading_position() {
        // A commit whose subject does not start with an id: no match, even though the
        // subject contains something id-shaped later.
        assert_eq!(landing_id("merge local/main (sp-htrqk)"), None);
    }

    #[test]
    fn landing_id_non_landing_commit_is_none() {
        assert_eq!(landing_id("update README"), None);
    }

    #[test]
    fn landing_id_spira_land_precedence_over_prefix() {
        // A subject matching both the "spira: land " prefix and looking prefix-colon-shaped
        // must take the land-form branch (awk's `next` after the first match).
        assert_eq!(landing_id("spira: land sp-abcde"), Some("sp-abcde".to_string()));
    }

    #[test]
    fn parse_landing_ids_dedupes_merge_and_landing_commit() {
        let subjects = "spira: land sp-herv0\nsp-herv0: mark the four use cases it uncovers\nspira: land sp-k6m1m\n";
        assert_eq!(parse_landing_ids(subjects), vec!["sp-herv0", "sp-k6m1m"]);
    }

    #[test]
    fn parse_landing_ids_real_fixture_window() {
        // Captured verbatim from `git log --format=%s` on this repository's own history.
        let subjects = "\
spira: land sp-herv0
sp-herv0: mark the four use cases it uncovers
sp-herv0: delete flipping suite test-watch-notify.sh
spira: land sp-k6m1m
sp-k6m1m: merge local/main
sp-k6m1m: merge local/main
sp-k6m1m: test-sending's stub store answers the list positive control
spira: land sp-z61hj
sp-k6m1m: stub stores answer the list positive control; boundary README regenerated
sp-z61hj: merge local/main (sp-htrqk)
spira: land sp-htrqk
sp-htrqk: retire test-batch-red-main.sh — subject covered by queue crate unit tests
spira: land sp-ak7qm
sp-ak7qm: merge local/main
spira: land sp-uy2gd
sp-uy2gd: delete flipping suite test-install-exec.sh
spira: land sp-s0e1k
";
        assert_eq!(
            parse_landing_ids(subjects),
            vec!["sp-herv0", "sp-k6m1m", "sp-z61hj", "sp-htrqk", "sp-ak7qm", "sp-uy2gd", "sp-s0e1k"]
        );
    }

    #[test]
    fn parse_repo_map_skips_comments_and_blanks() {
        let text = "# comment\n\nbrain | /home/ryan/spira/brain | push\nspira | /home/ryan/spira/harness | hold\n";
        assert_eq!(
            parse_repo_map(text),
            vec![
                ("brain".to_string(), "/home/ryan/spira/brain".to_string()),
                ("spira".to_string(), "/home/ryan/spira/harness".to_string()),
            ]
        );
    }

    #[test]
    fn parse_repo_map_trims_whitespace() {
        let text = "  brain   |   /path/to/brain   | push\n";
        assert_eq!(parse_repo_map(text), vec![("brain".to_string(), "/path/to/brain".to_string())]);
    }

    #[test]
    fn time_trigger_fires_at_threshold() {
        assert!(time_trigger(10800, 10800));
        assert!(!time_trigger(10799, 10800));
    }

    #[test]
    fn landing_trigger_fires_at_threshold() {
        assert!(landing_trigger(25, 25));
        assert!(!landing_trigger(24, 25));
    }

    #[test]
    fn invalid_closed_rows_filters_and_dedupes() {
        let raw = "ALLOWED-IC sp-aaa — quoted\nINVALID-CLOSED sp-bbb — close reason contains 'maybe': title\nUNFILED-FOLLOW sp-ccc — follow-on phrase: title\nINVALID-CLOSED sp-bbb — repeated\n";
        assert_eq!(
            invalid_closed_rows(raw),
            vec![
                "INVALID-CLOSED sp-bbb — close reason contains 'maybe': title".to_string(),
                "UNFILED-FOLLOW sp-ccc — follow-on phrase: title".to_string(),
            ]
        );
    }

    #[test]
    fn invalid_closed_rows_empty_on_no_rows() {
        assert_eq!(invalid_closed_rows(""), Vec::<String>::new());
        assert_eq!(invalid_closed_rows("ALLOWED-IC sp-aaa — quoted\n"), Vec::<String>::new());
    }

    #[test]
    fn trigger_labels_with_and_without_scope() {
        assert_eq!(trigger_labels("", "maechen-sweep"), "maechen-sweep");
        assert_eq!(trigger_labels("spira", "maechen-sweep"), "spira,maechen-sweep");
    }

    #[test]
    fn reason_text_orders_time_landing_invalid_closed() {
        let r = TriggerReason { time: true, landing: true, invalid_closed: true };
        assert_eq!(reason_text(&r, 11000, 30, 2), "11000s elapsed, 30 landings, 2 invalid-closed row(s)");
    }

    #[test]
    fn reason_text_only_landing() {
        let r = TriggerReason { time: false, landing: true, invalid_closed: false };
        assert_eq!(reason_text(&r, 0, 30, 0), "30 landings");
    }

    #[test]
    fn trigger_reason_any() {
        assert!(!TriggerReason { time: false, landing: false, invalid_closed: false }.any());
        assert!(TriggerReason { time: true, landing: false, invalid_closed: false }.any());
    }

    #[test]
    fn description_without_invalid_closed() {
        let d = description(3, "30 landings", 100, 50, None);
        assert!(d.starts_with("Scheduled trigger:"));
        assert!(d.contains("at most 3 remedy beads"));
        assert!(d.contains("Trigger: 30 landings (lastpass ts=100, watermark ts=50)"));
        assert!(!d.contains("Closed-record rows"));
    }

    #[test]
    fn description_with_invalid_closed_rows() {
        let d = description(3, "2 invalid-closed row(s)", 100, 50, Some("INVALID-CLOSED sp-bbb — x"));
        assert!(d.contains("Closed-record rows (detect_invalid_closed output):\nINVALID-CLOSED sp-bbb — x"));
    }
}
