//! Unit tests for `counters.rs` — the pure pieces (SQL shape, record format, streak
//! arithmetic, metadata/label parsing). The one process boundary each public function
//! crosses (`bd sql`/`bd show`/`bd update`, the filesystem, `date`) is exercised instead
//! by the real suites this bead's report names (test-attempts-sql.sh, test-census*.sh,
//! test-watchtower*.sh, test-thrash-teardown.sh) against a real bd store.

use super::*;

#[test]
fn fact_args_name_the_kind_actor_and_cause() {
    let a = fact_args("harness", "sp-a", "requeued", "merge-conflict");
    assert_eq!(a, ["fact", "sp-a", "--kind", "requeued", "--actor", "harness", "--cause", "merge-conflict"]);
}

#[test]
fn fact_args_strip_quotes_from_the_cause() {
    let a = fact_args("harness", "sp-a", "requeued", "o'hara\"; drop table events; --");
    assert_eq!(a[7], "ohara; drop table events; --");
}

#[test]
fn fact_args_bound_a_runaway_cause() {
    let a = fact_args("harness", "sp-a", "requeued", &"y".repeat(5000));
    assert!(a[7].len() <= 200, "{}", a[7].len());
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
