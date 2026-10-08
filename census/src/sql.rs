//! The SQL text census's own queries run — `_census_events_sql`/`_census_handwritten_sql`/
//! `_census_class_fold_map`/`_census_deliberate_sql`/`_census_deliberate_causes_sql_list`,
//! ported from `spira/lib.sh` (wave4-decomposition.md row M, wave 4.35, sp-kelr2).
//!
//! `census.sh` no longer exists for this to move "bash to bash" into — this crate replaced
//! it in an earlier bead, deliberately leaving the SQL behind in `lib.sh` (`ports.rs`'s own
//! doc: "The SQL itself... [is] UNCHANGED, still lib.sh"). "The family moves to its owning
//! crate" means here, in-process, now that the crate it is owned by is Rust.
//!
//! Pure string building throughout. `since_formatted`, where a since-watermark clause is
//! wanted, is an already-formatted `"YYYY-MM-DD HH:MM:SS"` UTC string — the caller resolves
//! the epoch through `World::format_epoch_utc` first, exactly as the bash shelled to `date`
//! itself; `None` (or the caller's own epoch <= 0, per the bash's own `-gt 0` guard) means
//! no clause at all, matching `_census_events_sql`'s and `_census_deliberate_sql`'s shared
//! shape: `[ -n "${1:-}" ] && [ "${1:-0}" -gt 0 ]`.
//!
//! Every string below was checked byte-for-byte against the live bash functions' own
//! output (several since-epoch values, with and without a watermark) before being ported —
//! see this bead's report for the exact transcript.
//!
//! THE DELIBERATE-CAUSE LIST IS NOT DUPLICATED HERE. `_census_deliberate_reopen_causes`
//! moved to `spira-claim` under row I (`bead_reopen`'s own admission-exemption decision,
//! sp-3wfcb, landed alongside this bead) before this module needed it, so
//! [`deliberate_causes_sql_list`]/[`events_sql`]/[`deliberate_sql`] take the cause names as
//! a parameter — fetched once, in `real.rs`, from `spira-claim deliberate-causes` (the same
//! CLI door `lib.sh`'s own `_census_deliberate_reopen_causes` shim now calls) — rather than
//! keeping a second copy that could drift from it (law-bake-rules-into-tools).

fn since_clause(since_formatted: Option<&str>) -> String {
    match since_formatted {
        Some(f) if !f.is_empty() => format!(" AND created_at > '{f}'"),
        _ => String::new(),
    }
}

/// `_census_deliberate_causes_sql_list` -> a quoted, comma-separated SQL IN-list, `causes`
/// already resolved by the caller. Escapes an embedded `'` the same way the bash's `sed
/// "s/'/''/g"` did — belt and suspenders, since every cause name today is a bare
/// identifier, but the escaping is cheap and was part of the original contract.
pub fn deliberate_causes_sql_list(causes: &[String]) -> String {
    causes.iter().map(|c| format!("'{}'", c.replace('\'', "''"))).collect::<Vec<_>>().join(", ")
}

/// `_census_events_sql [since_epoch_s]`.
pub fn events_sql(since_formatted: Option<&str>, causes: &[String]) -> String {
    events_sql_with(since_formatted, causes, &[])
}

/// [`events_sql`] for bd's table, whose `reopened` rows are paired with cause rows that now live
/// in the lifecycle log: `recorded` names the beads whose cause is there, so a reopen whose
/// cause row bd cannot see is not reported as unrecorded.
pub fn events_sql_with(since_formatted: Option<&str>, causes: &[String], recorded: &[String]) -> String {
    let recorded_clause = if recorded.is_empty() {
        String::new()
    } else {
        format!(" AND issue_id NOT IN ({})", deliberate_causes_sql_list(recorded))
    };
    let conflict_fold = "(event_type = 'requeued' AND new_value = 'merge-conflict')";
    let rebase_aeon_fold = "(event_type = 'requeued' AND new_value = 'rebase-conflict')";
    let eviction_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'eviction-race')";
    let prod_dirty_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'prod-dirty')";
    let unfinished_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'unfinished-reason')";
    let desc_hash_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'desc-changed-since-claim')";
    let reopen_timing_exclude = "(event_type = 'reopen' AND new_value IN ('closed-while-live', 'recurrence'))";
    let deliberate_fold = format!("(event_type = 'reopen' AND new_value IN ({}))", deliberate_causes_sql_list(causes));
    let unjudged_fold = format!(
        "(event_type = 'requeued' AND new_value LIKE 'unjudged-%' AND new_value <> 'unjudged-{}')",
        aeon::decide::CHARGING_OUTCOME
    );
    let actor_filter = "(actor = 'harness' OR actor LIKE 'aeon-%')";
    let sc = since_clause(since_formatted);

    let main_cond = format!(
        "event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen') AND NOT {conflict_fold} AND NOT (event_type = 'reopen' AND new_value = 'rebase-conflict') AND NOT {rebase_aeon_fold} AND NOT {eviction_fold} AND NOT {prod_dirty_fold} AND NOT {unfinished_fold} AND NOT {desc_hash_fold} AND NOT {unjudged_fold} AND NOT {reopen_timing_exclude} AND NOT {deliberate_fold} AND {actor_filter}{sc}"
    );
    let unrecorded_cond = format!(
        "event_type = 'reopened' AND {actor_filter}{sc} AND issue_id NOT IN (SELECT issue_id FROM events WHERE (event_type = 'reopen' OR {conflict_fold}){sc}){recorded_clause}"
    );
    let main = ranked_part("event_type, COALESCE(new_value, '')", "event_type, new_value", &main_cond, "event_type, new_value", "");
    let folded = [
        ("'reopen', 'rebase-conflict'", format!("((event_type = 'reopen' AND new_value = 'rebase-conflict') OR {conflict_fold} OR {rebase_aeon_fold}) AND {actor_filter}{sc}")),
        ("'reopen', 'eviction-race'", format!("{eviction_fold} AND {actor_filter}{sc}")),
        ("'reopen', 'prod-dirty'", format!("{prod_dirty_fold} AND {actor_filter}{sc}")),
        ("'reopen', 'unfinished-reason'", format!("{unfinished_fold} AND {actor_filter}{sc}")),
        ("'reopen', 'desc-changed-since-claim'", format!("{desc_hash_fold} AND {actor_filter}{sc}")),
        ("'reopened', 'unrecorded'", unrecorded_cond),
    ];
    let mut out = main;
    for (label, cond) in &folded {
        out.push_str(" UNION ALL ");
        out.push_str(&ranked_part(label, "", cond, "", " HAVING COUNT(DISTINCT issue_id) > 0"));
    }
    out.push_str(" ORDER BY 3 DESC");
    out
}

/// Events of one class closer together than this are one occurrence: a fleet-wide action
/// touching many beads at once is one failure, not one per bead.
const BURST_WINDOW_S: u32 = 60;

fn ranked_part(select_head: &str, partition: &str, cond: &str, group_by: &str, having: &str) -> String {
    let w = if partition.is_empty() { "ORDER BY created_at".to_string() } else { format!("PARTITION BY {partition} ORDER BY created_at") };
    let group = if group_by.is_empty() { String::new() } else { format!(" GROUP BY {group_by}") };
    format!(
        "SELECT {select_head}, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events, SUM(burst_start) AS bursts FROM (SELECT event_type, new_value, issue_id, CASE WHEN LAG(created_at) OVER ({w}) IS NULL OR TIMESTAMPDIFF(SECOND, LAG(created_at) OVER ({w}), created_at) > {BURST_WINDOW_S} THEN 1 ELSE 0 END AS burst_start FROM events WHERE {cond}) b{group}{having}"
    )
}

/// Same folds, exclusions, actor filter and since-bound as [`events_sql`], unaggregated: one
/// `(event_type, new_value, issue_id, unix-timestamp)` row per event. Clustering needs the
/// timestamps an aggregated `COUNT(*)` throws away.
pub fn event_rows_sql(since_formatted: Option<&str>, causes: &[String], recorded: &[String]) -> String {
    let recorded_clause = if recorded.is_empty() {
        String::new()
    } else {
        format!(" AND issue_id NOT IN ({})", deliberate_causes_sql_list(recorded))
    };
    let conflict_fold = "(event_type = 'requeued' AND new_value = 'merge-conflict')";
    let rebase_aeon_fold = "(event_type = 'requeued' AND new_value = 'rebase-conflict')";
    let eviction_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'eviction-race')";
    let prod_dirty_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'prod-dirty')";
    let unfinished_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'unfinished-reason')";
    let desc_hash_fold = "(event_type IN ('reopen', 'requeued') AND new_value = 'desc-changed-since-claim')";
    let reopen_timing_exclude = "(event_type = 'reopen' AND new_value IN ('closed-while-live', 'recurrence'))";
    let deliberate_fold = format!("(event_type = 'reopen' AND new_value IN ({}))", deliberate_causes_sql_list(causes));
    let unjudged_fold = format!(
        "(event_type = 'requeued' AND new_value LIKE 'unjudged-%' AND new_value <> 'unjudged-{}')",
        aeon::decide::CHARGING_OUTCOME
    );
    let actor_filter = "(actor = 'harness' OR actor LIKE 'aeon-%')";
    let sc = since_clause(since_formatted);

    format!(
        "SELECT event_type, COALESCE(new_value, ''), issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen') AND NOT {conflict_fold} AND NOT (event_type = 'reopen' AND new_value = 'rebase-conflict') AND NOT {rebase_aeon_fold} AND NOT {eviction_fold} AND NOT {prod_dirty_fold} AND NOT {unfinished_fold} AND NOT {reopen_timing_exclude} AND NOT {deliberate_fold} AND NOT {unjudged_fold} AND {actor_filter}{sc} UNION ALL SELECT 'reopen', 'rebase-conflict', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE ((event_type = 'reopen' AND new_value = 'rebase-conflict') OR {conflict_fold} OR {rebase_aeon_fold}) AND {actor_filter}{sc} UNION ALL SELECT 'reopen', 'eviction-race', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE {eviction_fold} AND {actor_filter}{sc} UNION ALL SELECT 'reopen', 'prod-dirty', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE {prod_dirty_fold} AND {actor_filter}{sc} UNION ALL SELECT 'reopen', 'unfinished-reason', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE {unfinished_fold} AND {actor_filter}{sc} UNION ALL SELECT 'reopen', 'desc-changed-since-claim', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE {desc_hash_fold} AND {actor_filter}{sc} UNION ALL SELECT 'reopened', 'unrecorded', issue_id, UNIX_TIMESTAMP(created_at) FROM events WHERE event_type = 'reopened' AND {actor_filter}{sc} AND issue_id NOT IN (SELECT issue_id FROM events WHERE (event_type = 'reopen' OR {conflict_fold}){sc}){recorded_clause} ORDER BY 1, 2, 4"
    )
}

/// `_census_handwritten_sql` — a fixed query, no since-watermark parameter.
pub fn handwritten_sql() -> String {
    "SELECT event_type, COALESCE(new_value, ''), actor, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen', 'reopened') AND NOT (actor = 'harness' OR actor LIKE 'aeon-%') GROUP BY event_type, new_value, actor ORDER BY 4 DESC".to_string()
}

/// `_census_deliberate_sql [since_epoch_s]`.
pub fn deliberate_sql(since_formatted: Option<&str>, causes: &[String]) -> String {
    format!(
        "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type = 'reopen' AND new_value IN ({}){} GROUP BY event_type, new_value ORDER BY 3 DESC",
        deliberate_causes_sql_list(causes),
        since_clause(since_formatted)
    )
}

/// `_census_class_fold_map` -> "<alias> <canonical>\n" lines, one per pair.
pub fn class_fold_map() -> String {
    const PAIRS: &[(&str, &str)] = &[
        ("sp-requeue-merge-conflict", "sp-reopen-rebase-conflict"),
        ("sp-requeue-rebase-conflict", "sp-reopen-rebase-conflict"),
        ("sp-requeue-eviction-race", "sp-reopen-eviction-race"),
        ("sp-requeue-prod-dirty", "sp-reopen-prod-dirty"),
        ("sp-requeue-unfinished-reason", "sp-reopen-unfinished-reason"),
        ("sp-requeue-unjudged-slain", "sp-requeue-unjudged-killed"),
        ("sp-requeue-workflow-run-missing", "sp-reopen-workflow-run-missing"),
        ("sp-requeue-workflow-run-wrong-branch", "sp-reopen-workflow-run-wrong-branch"),
        ("sp-requeue-workflow-run-stale-sha", "sp-reopen-workflow-run-stale-sha"),
        ("sp-requeue-workflow-run-wrong-file", "sp-reopen-workflow-run-wrong-file"),
        ("sp-requeue-workflow-run-unverifiable", "sp-reopen-workflow-run-unverifiable"),
        ("sp-requeue-desc-changed-since-claim", "sp-reopen-desc-changed-since-claim"),
    ];
    PAIRS.iter().map(|(a, b)| format!("{a} {b}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ground truth captured from the live bash functions (sourced lib.sh, same repo,
    // 2026-10-01) — see this bead's report for the exact transcript these were checked
    // against before being pasted in as the expected strings here. `causes()` stands in
    // for the `spira-claim deliberate-causes` fetch `real.rs` does in production — the
    // two deliberate causes as of this writing (sp-3wfcb, row I, landed alongside this
    // bead); `spira_claim::reopen::DELIBERATE_CAUSES` is the canonical source now.
    fn causes() -> Vec<String> {
        vec!["work-close-converted".to_string(), "eject".to_string()]
    }

    #[test]
    fn deliberate_causes_sql_list_escapes_an_embedded_quote() {
        let out = deliberate_causes_sql_list(&["o'brien".to_string()]);
        assert_eq!(out, "'o''brien'");
    }

    #[test]
    fn deliberate_causes_sql_list_matches_the_bash() {
        assert_eq!(deliberate_causes_sql_list(&causes()), "'work-close-converted', 'eject'");
    }

    #[test]
    fn handwritten_sql_matches_the_bash() {
        assert_eq!(
            handwritten_sql(),
            "SELECT event_type, COALESCE(new_value, ''), actor, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen', 'reopened') AND NOT (actor = 'harness' OR actor LIKE 'aeon-%') GROUP BY event_type, new_value, actor ORDER BY 4 DESC"
        );
    }

    #[test]
    fn deliberate_sql_with_no_watermark_matches_the_bash() {
        assert_eq!(
            deliberate_sql(None, &causes()),
            "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type = 'reopen' AND new_value IN ('work-close-converted', 'eject') GROUP BY event_type, new_value ORDER BY 3 DESC"
        );
    }

    #[test]
    fn deliberate_sql_with_a_watermark_matches_the_bash() {
        assert_eq!(
            deliberate_sql(Some("2023-11-14 22:13:20"), &causes()),
            "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type = 'reopen' AND new_value IN ('work-close-converted', 'eject') AND created_at > '2023-11-14 22:13:20' GROUP BY event_type, new_value ORDER BY 3 DESC"
        );
    }

    #[test]
    fn events_sql_collapses_bursts() {
        let sql = events_sql(None, &causes());
        assert!(sql.contains("SUM(burst_start) AS bursts"));
        assert!(sql.contains("> 60 THEN 1"));
        assert!(!sql.contains("created_at >"));
    }

    #[test]
    fn events_sql_applies_the_watermark_to_every_part() {
        let sql = events_sql(Some("2023-11-14 22:13:20"), &causes());
        assert_eq!(sql.matches("AND created_at > '2023-11-14 22:13:20'").count(), 8);
    }

    #[test]
    fn a_reopen_recorded_in_the_lifecycle_log_is_not_unrecorded() {
        let plain = events_sql(None, &causes());
        assert!(!plain.contains("issue_id NOT IN ('"), "no recorded list unless one is given");
        let with = events_sql_with(None, &causes(), &["sp-a".to_string(), "o'x".to_string()]);
        assert_eq!(with.matches("AND issue_id NOT IN ('sp-a', 'o''x')").count(), 1, "only the unrecorded branch carries it");
        assert!(with.contains("event_type = 'reopened'"));
    }

    #[test]
    fn events_sql_excludes_every_unjudged_cause_but_the_charging_one() {
        let q = events_sql(None, &causes());
        assert!(q.contains("new_value <> 'unjudged-unlanded'"));
        assert!(aeon::decide::outcome_charges("unlanded"));
        assert!(!aeon::decide::outcome_charges("operator-wait"));
    }

    #[test]
    fn event_rows_sql_is_unaggregated_and_keeps_the_exclusions_of_events_sql() {
        let rows = event_rows_sql(Some("2023-11-14 22:13:20"), &causes(), &[]);
        assert!(!rows.contains("COUNT("));
        assert!(rows.contains("UNIX_TIMESTAMP(created_at)"));
        let agg = events_sql(Some("2023-11-14 22:13:20"), &causes());
        for must in [
            "NOT (event_type = 'reopen' AND new_value IN ('closed-while-live', 'recurrence'))",
            "NOT (event_type = 'reopen' AND new_value IN ('work-close-converted', 'eject'))",
            "new_value <> 'unjudged-unlanded'",
            "(actor = 'harness' OR actor LIKE 'aeon-%')",
            "created_at > '2023-11-14 22:13:20'",
        ] {
            assert!(rows.contains(must), "{must}");
            assert!(agg.contains(must), "{must}");
        }
    }

    #[test]
    fn class_fold_map_matches_the_bash() {
        assert_eq!(
            class_fold_map(),
            "sp-requeue-merge-conflict sp-reopen-rebase-conflict\n\
             sp-requeue-rebase-conflict sp-reopen-rebase-conflict\n\
             sp-requeue-eviction-race sp-reopen-eviction-race\n\
             sp-requeue-prod-dirty sp-reopen-prod-dirty\n\
             sp-requeue-unfinished-reason sp-reopen-unfinished-reason\n\
             sp-requeue-unjudged-slain sp-requeue-unjudged-killed\n\
             sp-requeue-workflow-run-missing sp-reopen-workflow-run-missing\n\
             sp-requeue-workflow-run-wrong-branch sp-reopen-workflow-run-wrong-branch\n\
             sp-requeue-workflow-run-stale-sha sp-reopen-workflow-run-stale-sha\n\
             sp-requeue-workflow-run-wrong-file sp-reopen-workflow-run-wrong-file\n\
             sp-requeue-workflow-run-unverifiable sp-reopen-workflow-run-unverifiable\n\
             sp-requeue-desc-changed-since-claim sp-reopen-desc-changed-since-claim\n"
        );
    }
}
