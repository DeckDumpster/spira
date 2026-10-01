//! Unit tests for `counters.rs` — the pure pieces (SQL shape, record format, streak
//! arithmetic, metadata/label parsing). The one process boundary each public function
//! crosses (`bd sql`/`bd show`/`bd update`, the filesystem, `date`) is exercised instead
//! by the real suites this bead's report names (test-attempts-sql.sh, test-census*.sh,
//! test-watchtower*.sh, test-thrash-teardown.sh) against a real bd store.

use super::*;

#[test]
fn insert_sql_shape() {
    let q = insert_sql("u1", "sp-a", "requeued", "harness", "merge-conflict");
    assert!(q.starts_with("INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ("));
    assert!(q.contains("UTC_TIMESTAMP()"), "{q}");
    assert!(!q.contains("NOW()"), "sp-yyih8: must never use server-local NOW(): {q}");
    assert!(q.contains("'u1'") && q.contains("'sp-a'") && q.contains("'requeued'") && q.contains("'harness'") && q.contains("'merge-conflict'"));
}

#[test]
fn insert_sql_strips_quotes_from_the_cause() {
    // bash's own version interpolated the cause unescaped, a latent injection risk this
    // port closes two ways: bounded_cause strips quote/backslash/control characters
    // before store::sql_quote ever sees the value, so there is nothing left to escape.
    let q = insert_sql("u1", "sp-a", "requeued", "harness", "o'hara\"; drop table events; --");
    assert!(q.contains("'ohara; drop table events; --'"), "{q}");
}

#[test]
fn count_sql_shape() {
    let q = count_sql("sp-a", "__doctor_probe__");
    assert_eq!(q, "SELECT COUNT(*) FROM events WHERE issue_id='sp-a' AND event_type='__doctor_probe__'");
}

#[test]
fn parse_scalar_count_reads_the_third_line() {
    // bd sql's own 3-row shape: header, separator, data (test-census-pipeline.sh's own
    // comment on the same shape: "header/separator/data").
    let out = "count(*)\n--------\n7\n";
    assert_eq!(parse_scalar_count(out), Some(7));
}

#[test]
fn parse_scalar_count_strips_padding() {
    let out = "count(*)\n--------\n   42  \n";
    assert_eq!(parse_scalar_count(out), Some(42));
}

#[test]
fn parse_scalar_count_none_on_garbage() {
    assert_eq!(parse_scalar_count("\n\n"), None);
    assert_eq!(parse_scalar_count("a\nb\nnot-a-number\n"), None);
}

#[test]
fn lapse_record_content_matches_the_reader_format() {
    let got = lapse_record_content("sp-x", "600", "writing output file", "abc1234");
    assert_eq!(got, "bead: sp-x\nquiet: 600s\nlast: writing output file\nbranch: spira/sp-x\ntip: abc1234\n");
}

#[test]
fn next_streak_same_tip_increments() {
    assert_eq!(next_streak("abc123", 2, "abc123"), 3);
}

#[test]
fn next_streak_different_tip_resets() {
    assert_eq!(next_streak("abc123", 5, "def456"), 1);
}

#[test]
fn next_streak_first_ever_is_one() {
    assert_eq!(next_streak("", 0, "abc123"), 1);
}

#[test]
fn next_streak_unknown_tip_never_streaks() {
    // "?" means "tip unknown" (the aeon could not read it) — never treated as a repeat,
    // even against a previous "?" (which would otherwise look like a matching pair).
    assert_eq!(next_streak("?", 3, "?"), 1);
    assert_eq!(next_streak("abc", 3, ""), 1);
}

#[test]
fn truncate_note_folds_newlines_and_cuts_at_300() {
    let note = format!("line one\r\nline two{}", "x".repeat(400));
    let got = truncate_note(&note);
    assert_eq!(got.len(), 300);
    assert!(!got.contains('\n') && !got.contains('\r'));
    assert!(got.starts_with("line one  line two"));
}

#[test]
fn counter_label_exact_match() {
    let labels = vec!["spira".to_string(), "sp-attempt-3".to_string(), "plan".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), Some("sp-attempt-3".to_string()));
}

#[test]
fn counter_label_suffixed_match() {
    let labels = vec!["sp-attempt-3-unlanded".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), Some("sp-attempt-3-unlanded".to_string()));
}

#[test]
fn counter_label_no_match() {
    let labels = vec!["sp-attempt-2".to_string(), "spira".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), None);
}

#[test]
fn counter_label_does_not_match_a_longer_number() {
    // "sp-attempt-3" must not match a search for prefix "sp-attempt" n "30" and vice
    // versa — the exact-or-dash-suffixed rule, not a bare prefix scan.
    let labels = vec!["sp-attempt-30".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), None);
}

#[test]
fn parse_metadata_reads_the_long_json() {
    let text = r#"[{"id":"sp-a","metadata":{"thrash_tip":"abc123","thrash_streak":"2"}}]"#;
    let m = parse_metadata(text);
    assert_eq!(m.get("thrash_tip").and_then(Value::as_str), Some("abc123"));
    assert_eq!(m.get("thrash_streak").and_then(Value::as_str), Some("2"));
}

#[test]
fn parse_metadata_skips_a_leading_warning_line() {
    let text = "bd: warning: something\n[{\"id\":\"sp-a\",\"metadata\":{\"k\":\"v\"}}]";
    let m = parse_metadata(text);
    assert_eq!(m.get("k").and_then(Value::as_str), Some("v"));
}

#[test]
fn parse_metadata_absent_is_empty_not_an_error() {
    assert!(parse_metadata(r#"[{"id":"sp-a"}]"#).is_empty());
    assert!(parse_metadata("not json at all").is_empty());
    assert!(parse_metadata("").is_empty());
}
