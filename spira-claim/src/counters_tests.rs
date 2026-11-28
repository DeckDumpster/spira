//! Unit tests for `counters.rs` — the pure pieces (SQL shape, record format, streak
//! arithmetic, metadata/label parsing). The one process boundary each public function
//! crosses (`bd sql`/`bd show`/`bd update`, the filesystem, `date`) is exercised instead
//! by the real suites this bead's report names (test-census*.sh) against a real bd store; the
//! record format's reader is `watchtower::lapsed::tests`.

use super::*;

#[test_id(tpwbnzs)]
#[test]
fn fact_args_name_the_kind_actor_and_cause() {
    let a = fact_args("harness", "sp-a", "requeued", "merge-conflict");
    assert_eq!(a, ["fact", "add", "sp-a", "--kind", "requeued", "--actor", "harness", "--cause", "merge-conflict"]);
}

#[test_id(tg5j5fi)]
#[test]
fn fact_args_strip_quotes_from_the_cause() {
    let a = fact_args("harness", "sp-a", "requeued", "o'hara\"; drop table events; --");
    assert_eq!(a[8], "ohara; drop table events; --");
}

#[test_id(tgbdvec)]
#[test]
fn fact_args_bound_a_runaway_cause() {
    let a = fact_args("harness", "sp-a", "requeued", &"y".repeat(5000));
    assert!(a[8].len() <= 200, "{}", a[8].len());
}

#[test_id(txik2q9)]
#[test]
fn lapse_record_content_matches_the_reader_format() {
    let got = lapse_record_content("sp-x", "600", "writing output file", "abc1234");
    assert_eq!(got, "bead: sp-x\nquiet: 600s\nlast: writing output file\nbranch: spira/sp-x\ntip: abc1234\n");
}

#[test_id(tks5ugq)]
#[test]
fn next_streak_same_tip_increments() {
    assert_eq!(next_streak("abc123", 2, "abc123"), 3);
}

#[test_id(tsrhtgn)]
#[test]
fn next_streak_different_tip_resets() {
    assert_eq!(next_streak("abc123", 5, "def456"), 1);
}

#[test_id(tx5cutp)]
#[test]
fn next_streak_first_ever_is_one() {
    assert_eq!(next_streak("", 0, "abc123"), 1);
}

#[test_id(tcvvrgh)]
#[test]
fn next_streak_unknown_tip_never_streaks() {
    // "?" means "tip unknown" (the dwarf could not read it) — never treated as a repeat,
    // even against a previous "?" (which would otherwise look like a matching pair).
    assert_eq!(next_streak("?", 3, "?"), 1);
    assert_eq!(next_streak("abc", 3, ""), 1);
}

#[test_id(t81arwc)]
#[test]
fn truncate_note_folds_newlines_and_cuts_at_300() {
    let note = format!("line one\r\nline two{}", "x".repeat(400));
    let got = truncate_note(&note);
    assert_eq!(got.len(), 300);
    assert!(!got.contains('\n') && !got.contains('\r'));
    assert!(got.starts_with("line one  line two"));
}

#[test_id(tvf8jc9)]
#[test]
fn counter_label_exact_match() {
    let labels = vec!["spira".to_string(), "sp-attempt-3".to_string(), "plan".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), Some("sp-attempt-3".to_string()));
}

#[test_id(t94oyh1)]
#[test]
fn counter_label_suffixed_match() {
    let labels = vec!["sp-attempt-3-unlanded".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), Some("sp-attempt-3-unlanded".to_string()));
}

#[test_id(teeugaf)]
#[test]
fn counter_label_no_match() {
    let labels = vec!["sp-attempt-2".to_string(), "spira".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), None);
}

#[test_id(thmtnha)]
#[test]
fn counter_label_does_not_match_a_longer_number() {
    // "sp-attempt-3" must not match a search for prefix "sp-attempt" n "30" and vice
    // versa — the exact-or-dash-suffixed rule, not a bare prefix scan.
    let labels = vec!["sp-attempt-30".to_string()];
    assert_eq!(counter_label(&labels, "sp-attempt", "3"), None);
}

#[test_id(tegvset)]
#[test]
fn the_streak_metadata_is_written_through_the_content_door() {
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    struct Fake(Mutex<Vec<Vec<String>>>);
    impl Transport for Fake {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(Reply { code: 0, out: String::new(), err: String::new(), read_ended: false })
        }
    }
    let fake = Arc::new(Fake(Mutex::new(Vec::new())));
    let store = Store { lc: LcClient::over(fake.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: std::time::Duration::from_secs(1), views: None, readiness_alert: Default::default() };
    thrash_streak_bump(&store, "sp-a", "?", "stalled");
    let seen = fake.0.lock().unwrap().clone();
    let writes: Vec<_> = seen.iter().filter(|a| a[..2] == ["content", "update"]).collect();
    assert_eq!(writes.len(), 1, "{seen:?}");
    assert_eq!(writes[0], &["content", "update", "sp-a", "--set-metadata", "thrash_tip=?", "--set-metadata", "thrash_streak=1", "--set-metadata", "thrash_last=stalled"]);
}
