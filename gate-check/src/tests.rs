//! Unit tests over `engine.rs`'s pure decision surface (DESIGN.md "Test strategy").

use super::*;

// ------------------------------------------------------------------------ discover_branches()

#[test]
fn discover_branches_from_open_unbound_gh_run_gates() {
    let json = r#"[
        {"await_type":"gh:run","await_id":"","metadata":{"branch":"spira/sp-1"}},
        {"await_type":"gh:run","await_id":"","metadata":{"branch":"spira/sp-2"}},
        {"await_type":"gh:run","await_id":"","metadata":{"branch":"spira/sp-1"}},
        {"await_type":"gh:run","await_id":"12345","metadata":{"branch":"spira/sp-3"}},
        {"await_type":"other","await_id":"","metadata":{"branch":"spira/sp-4"}}
    ]"#;
    assert_eq!(discover_branches(json), vec!["spira/sp-1", "spira/sp-2"]);
}

#[test]
fn discover_branches_tolerates_null_and_non_array() {
    assert_eq!(discover_branches("null"), Vec::<String>::new());
    assert_eq!(discover_branches("not json"), Vec::<String>::new());
    // A single object, not wrapped in an array (the bash's `[] if gates is None else [gates]`).
    let json = r#"{"await_type":"gh:run","await_id":"","metadata":{"branch":"x"}}"#;
    assert_eq!(discover_branches(json), vec!["x"]);
}

#[test]
fn discover_branches_skips_entries_missing_a_branch() {
    let json = r#"[{"await_type":"gh:run","await_id":"","metadata":{}}]"#;
    assert!(discover_branches(json).is_empty());
}

// ------------------------------------------------------------------------------ stuck_count()

#[test]
fn stuck_count_matches_the_exact_bd_line() {
    let out = "gate resolved: sp-1\nerror: no run ID specified for sp-2\nfine\nno run ID specified\n";
    assert_eq!(stuck_count(out), 2);
}

#[test]
fn stuck_count_zero_when_absent() {
    assert_eq!(stuck_count("all good\n"), 0);
}

// -------------------------------------------------------------------------- escalate_lines()

#[test]
fn escalate_lines_finds_only_escalate_lines() {
    let out = "sp-1: ok\n⚠ sp-2: ESCALATE ci red\nsp-3: ok\n⚠ sp-4: ESCALATE base red\n";
    assert_eq!(escalate_lines(out), vec!["⚠ sp-2: ESCALATE ci red", "⚠ sp-4: ESCALATE base red"]);
}

// ------------------------------------------------------------------------------- gate_id_in()

#[test]
fn gate_id_in_extracts_first_match_no_prefix_assumed() {
    assert_eq!(gate_id_in("⚠ sp-9tal: ESCALATE ci red"), Some("sp-9tal".to_string()));
    assert_eq!(gate_id_in("⚠ custom-abc123: ESCALATE"), Some("custom-abc123".to_string()));
}

#[test]
fn gate_id_in_none_when_no_id_shaped_token() {
    assert_eq!(gate_id_in("ESCALATE with nothing that looks like an id"), None);
}

// ------------------------------------------------------------------------------ blocked_bead()

#[test]
fn blocked_bead_extracts_the_named_id() {
    let json = r#"[{"description":"gate blocking sp-abc12 until CI resolves"}]"#;
    assert_eq!(blocked_bead(json), Some("sp-abc12".to_string()));
}

#[test]
fn blocked_bead_handles_a_bare_object_not_a_list() {
    let json = r#"{"description":"blocking sp-xyz99"}"#;
    assert_eq!(blocked_bead(json), Some("sp-xyz99".to_string()));
}

#[test]
fn blocked_bead_none_when_description_has_no_blocking_clause() {
    let json = r#"[{"description":"nothing relevant here"}]"#;
    assert_eq!(blocked_bead(json), None);
}

#[test]
fn blocked_bead_none_on_malformed_json() {
    assert_eq!(blocked_bead("not json"), None);
}

// -------------------------------------------------------------------------- has/find open bead

fn lc(rows: &[(&str, &str)]) -> Lc {
    rows.iter()
        .map(|(id, st)| (id.to_string(), spira_config::lc_state::Row { bead_id: id.to_string(), state: st.to_string(), ..Default::default() }))
        .collect()
}

/// sp-mve9i: a filed bead is open while its lifecycle row is not terminal; bd's status (here
/// deliberately the opposite of each row's state) plays no part.
#[test]
fn has_open_bead_follows_the_lifecycle_row_with_exact_title() {
    let json = r#"[
        {"id":"sp-a","status":"open","title":"flaky suite: a.sh"},
        {"id":"sp-b","status":"closed","title":"flaky suite: b.sh"},
        {"id":"sp-c","status":"closed","title":"flaky suite: c.sh"},
        {"id":"sp-d","status":"open","title":"flaky suite: d.sh"}
    ]"#;
    let lc = lc(&[("sp-a", "LANDED"), ("sp-b", "WORKING"), ("sp-c", "SUBMITTED")]);
    assert!(!has_open_bead(json, "flaky suite: a.sh", &lc));
    assert!(has_open_bead(json, "flaky suite: b.sh", &lc));
    assert!(has_open_bead(json, "flaky suite: c.sh", &lc));
    assert!(!has_open_bead(json, "flaky suite: d.sh", &lc), "no row: never worked, holds nothing open");
    assert!(!has_open_bead(json, "flaky suite: nonexistent.sh", &lc));
}

#[test]
fn find_open_bead_returns_id_and_default_priority() {
    let json = r#"[{"id":"sp-1","title":"suite red on main: x.sh"}]"#;
    assert_eq!(find_open_bead(json, "suite red on main: x.sh", &lc(&[("sp-1", "READY")])), Some(("sp-1".to_string(), 2)));
}

#[test]
fn find_open_bead_reads_explicit_priority() {
    let json = r#"[{"id":"sp-1","title":"t","priority":1}]"#;
    assert_eq!(find_open_bead(json, "t", &lc(&[("sp-1", "REWORK")])), Some(("sp-1".to_string(), 1)));
}

#[test]
fn find_open_bead_none_when_not_present() {
    assert_eq!(find_open_bead("[]", "t", &lc(&[])), None);
}

// ---------------------------------------------------------------------------------- job_ids()

#[test]
fn job_ids_from_jobs_array() {
    let json = r#"{"jobs":[{"id":111},{"id":222},{"name":"no id field"}]}"#;
    assert_eq!(job_ids(json), vec![111, 222]);
}

#[test]
fn job_ids_empty_on_malformed_or_missing() {
    assert!(job_ids("not json").is_empty());
    assert!(job_ids("{}").is_empty());
}

// ------------------------------------------------------------------------------ flaky_suites()

#[test]
fn flaky_suites_extracts_name_up_to_was_red() {
    let json = r#"[{"annotation_level":"warning","title":"flaky suite","message":"test-foo.sh was red on a parallel run"}]"#;
    assert_eq!(flaky_suites(json), vec!["test-foo.sh"]);
}

#[test]
fn flaky_suites_skips_wrong_level_or_title() {
    let json = r#"[
        {"annotation_level":"failure","title":"flaky suite","message":"x.sh was red"},
        {"annotation_level":"warning","title":"other","message":"y.sh was red"}
    ]"#;
    assert!(flaky_suites(json).is_empty());
}

#[test]
fn flaky_suites_skips_when_marker_not_found_or_at_index_zero() {
    let json = r#"[{"annotation_level":"warning","title":"flaky suite","message":"no marker here"}]"#;
    assert!(flaky_suites(json).is_empty());
    let json2 = r#"[{"annotation_level":"warning","title":"flaky suite","message":" was red with nothing before it"}]"#;
    assert!(flaky_suites(json2).is_empty());
}

// --------------------------------------------------------------------------- red_twice_suites()

#[test]
fn red_twice_suites_full_message_untruncated() {
    let json = r#"[{"annotation_level":"failure","title":"red-twice suite","message":"test-bar.sh"}]"#;
    assert_eq!(red_twice_suites(json), vec!["test-bar.sh"]);
}

// ---------------------------------------------------------------------------- titles / bodies

#[test]
fn titles_match_the_bash_prefixes() {
    assert_eq!(flaky_title("x.sh"), "flaky suite: x.sh");
    assert_eq!(red_twice_title("x.sh"), "suite red on main: x.sh");
}

#[test]
fn flaky_body_shape() {
    let b = flaky_body("12345", "test-foo.sh");
    assert_eq!(b, "Flaky suite in run 12345.\n\ntest-foo.sh was red on a parallel run then green on a serial re-run.\n");
}

#[test]
fn red_twice_body_fills_placeholders_when_empty() {
    let b = red_twice_body("x.sh", "999", "", "", "", "");
    assert!(b.contains("First red commit: ?"));
    assert!(b.contains("Last green commit: (unknown)"));
    assert!(b.contains("Failing assertions:\n(none captured)"));
    assert!(b.contains("Commits in range:\n(range unknown)"));
}

#[test]
fn red_twice_body_fills_real_values() {
    let b = red_twice_body("x.sh", "999", "abc123", "def456", "FAIL: boom", "abc123 fix stuff");
    assert!(b.starts_with("x.sh is red on main and blocks the release.\n"));
    assert!(b.contains("Run: 999"));
    assert!(b.contains("First red commit: abc123"));
    assert!(b.contains("Last green commit: def456"));
    assert!(b.contains("FAIL: boom"));
    assert!(b.contains("abc123 fix stuff"));
}
