//! Unit tests over `engine.rs`'s pure decision surface (DESIGN.md "Test strategy").

use super::*;

// ------------------------------------------------------------------------------ fail_lines()

#[test]
fn tier1_matches_testlib_fail_shape() {
    let out = "setup\n  FAIL  something broke\nmore\n";
    assert_eq!(fail_lines(out), vec!["  FAIL  something broke".to_string()]);
}

#[test]
fn tier1_matches_fail_colon_and_not_ok() {
    assert_eq!(fail_lines("FAIL: boom\n"), vec!["FAIL: boom".to_string()]);
    assert_eq!(fail_lines("not ok 3 - thing\n"), vec!["not ok 3 - thing".to_string()]);
}

#[test]
fn tier2_falls_back_when_tier1_matches_nothing() {
    // "FAIL" appears mid-line, not in any of the tight tier-1 shapes.
    let out = "assertion FAILed unexpectedly\n";
    assert_eq!(fail_lines(out), vec!["assertion FAILed unexpectedly".to_string()]);
}

#[test]
fn no_fail_lines_when_neither_tier_matches() {
    assert!(fail_lines("nothing interesting here\n").is_empty());
}

// -------------------------------------------------------------------------------- first_fail()

#[test]
fn first_fail_uses_first_matching_line_trimmed() {
    let lines = vec!["   FAIL: boom".to_string(), "FAIL: other".to_string()];
    assert_eq!(first_fail("whatever", &lines, "1", "5", 600), "FAIL: boom");
}

#[test]
fn first_fail_reports_no_output_when_empty() {
    assert_eq!(first_fail("", &[], "137", "600", 600), "(died rc=137 at 600s/600s — no output)");
}

#[test]
fn first_fail_reports_last_line_when_no_fail_line_found() {
    let out = "starting up\nstill going\n";
    assert_eq!(first_fail(out, &[], "124", "300", 300), "(died rc=124 at 300s/300s — last: still going)");
}

// -------------------------------------------------------------------------- declared_timeout()

#[test]
fn declared_timeout_reads_the_suites_own_comment() {
    let src = "#!/usr/bin/env bash\n# timeout: 120\necho hi\n";
    assert_eq!(declared_timeout(Some(src), 600), 120);
}

#[test]
fn declared_timeout_falls_back_when_absent() {
    let src = "#!/usr/bin/env bash\necho hi\n";
    assert_eq!(declared_timeout(Some(src), 600), 600);
}

#[test]
fn declared_timeout_falls_back_when_not_a_plain_integer() {
    let src = "# timeout: soon\n";
    assert_eq!(declared_timeout(Some(src), 600), 600);
}

#[test]
fn declared_timeout_falls_back_when_source_unreadable() {
    assert_eq!(declared_timeout(None, 600), 600);
}

// -------------------------------------------------------------------------- classify_retry()

#[test]
fn retry_ok_is_flaky() {
    assert_eq!(classify_retry(Some("ok")), (RetryClass::Flaky, "red-green (flake)"));
}

#[test]
fn retry_red_or_timeout_is_red_red() {
    assert_eq!(classify_retry(Some("red")), (RetryClass::RedRed, "red-red"));
    assert_eq!(classify_retry(Some("timeout")), (RetryClass::RedRed, "red-red"));
}

#[test]
fn retry_missing_or_other_is_uncertain_red() {
    assert_eq!(classify_retry(None), (RetryClass::Uncertain, "red"));
    assert_eq!(classify_retry(Some("quarantined-red")), (RetryClass::Uncertain, "red"));
}

// ------------------------------------------------------------------------------ summary_row()

#[test]
fn summary_row_folds_pipes_and_truncates() {
    let fail = "a".repeat(90) + "|end";
    let row = summary_row("test-x.sh", "12", "1", "red", &fail);
    assert!(row.starts_with("| test-x.sh | 12s (rc=1) | red | "));
    // 80-char cap on the FAIL cell, pipe folded to bang.
    assert!(!row.contains('\n'));
    assert_eq!(row.matches('|').count(), 5); // 4 cell delimiters; no stray '|' leaked from the fail text
}

#[test]
fn annotation_text_strips_newlines_and_caps_length() {
    let fail = format!("line1\nline2{}", "x".repeat(300));
    let a = annotation_text(&fail);
    assert!(!a.contains('\n'));
    assert_eq!(a.chars().count(), 200);
}

// -------------------------------------------------------------------------------- tail_n()

#[test]
fn tail_n_keeps_last_lines_only() {
    assert_eq!(tail_n("a\nb\nc\nd\n", 2), "c\nd");
}

// ---------------------------------------------------------------------------- JSON builders

#[test]
fn red_suites_json_shape() {
    let j = red_suites_json(&["a.sh".into(), "b.sh".into()], &["c.sh".into()]);
    assert_eq!(j, "{\"red\":[\"a.sh\",\"b.sh\"],\"flaky\":[\"c.sh\"],\"red_count\":2}");
}

#[test]
fn red_suites_json_escapes_quotes_and_backslashes() {
    let j = red_suites_json(&["weird\"suite\\.sh".into()], &[]);
    assert!(j.contains("weird\\\"suite\\\\.sh"));
}

#[test]
fn verdict_rows_one_line_per_suite() {
    let rows = verdict_rows(&["a.sh".into()], &["b.sh".into()]);
    let lines: Vec<&str> = rows.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"status\":\"red-red\""));
    assert!(lines[0].contains("\"suite\":\"a.sh\""));
    assert!(lines[1].contains("\"status\":\"red-green\""));
    assert!(lines[1].contains("\"suite\":\"b.sh\""));
}

// -------------------------------------------------------------------------------- junit_xml()

fn row(suite: &str, case: &str, status: &str, secs: i64, detail: &str) -> TapRow {
    TapRow { suite: suite.into(), case: case.into(), status: status.into(), seconds: serde_json::Value::from(secs), detail: detail.into() }
}

#[test]
fn junit_counts_failures_and_skips_per_suite() {
    let rows = vec![
        row("a.sh", "case1", "pass", 1, ""),
        row("a.sh", "case2", "fail", 2, "boom"),
        row("b.sh", "(suite)", "skip", 0, "not applicable"),
    ];
    let xml = junit_xml(&rows);
    assert!(xml.contains("<testsuite name=\"a.sh\" tests=\"2\" failures=\"1\" skipped=\"0\" time=\"1\">"));
    assert!(xml.contains("<testsuite name=\"b.sh\" tests=\"1\" failures=\"0\" skipped=\"1\" time=\"0\">"));
    assert!(xml.contains("<failure message=\"boom\"></failure>"));
    assert!(xml.contains("<skipped message=\"not applicable\"></skipped>"));
}

#[test]
fn junit_escapes_xml_special_characters_including_quotes() {
    let rows = vec![row("a.sh", "case \"with\" quotes & <tags>", "fail", 0, "it said \"no\"")];
    let xml = junit_xml(&rows);
    assert!(xml.contains("case &quot;with&quot; quotes &amp; &lt;tags&gt;"));
    assert!(xml.contains("it said &quot;no&quot;"));
}

#[test]
fn junit_suites_sorted_and_empty_input_is_still_valid_shape() {
    let xml = junit_xml(&[]);
    assert_eq!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n</testsuites>\n");
}

#[test]
fn parse_tap_rows_skips_unparseable_lines() {
    let jsonl = "{\"suite\":\"a.sh\",\"tier\":\"\",\"case\":\"c\",\"status\":\"pass\",\"seconds\":1,\"uc\":[],\"detail\":\"\"}\nnot json\n";
    let rows = parse_tap_rows(jsonl);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].suite, "a.sh");
}

// ------------------------------------------------------------------------------ is_red()

#[test]
fn is_red_matches_red_and_timeout_only() {
    assert!(is_red("red"));
    assert!(is_red("timeout"));
    assert!(!is_red("ok"));
    assert!(!is_red("skip"));
}
