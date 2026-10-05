use super::*;

fn o(suite: &str, v: Verdict) -> Outcome {
    Outcome { suite: suite.into(), verdict: v, secs: Some(3) }
}

fn run(history: &mut Vec<Row>, res: &[Outcome], commit: &str, round: &str, mode: &str) -> Vec<Event> {
    let (rows, ev) = record(history, res, commit, round, mode);
    history.extend(rows);
    ev
}

#[test]
fn a_planted_red_is_reported_with_the_window_between_last_green_and_first_red() {
    let mut h = Vec::new();
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Ok), o("test-b.sh", Verdict::Ok)], "c183", "183", "seed").is_empty());
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Ok), o("test-b.sh", Verdict::Ok)], "c184", "184", "full").is_empty());
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Ok), o("test-b.sh", Verdict::Red)], "c185", "185", "full");
    assert_eq!(
        ev,
        vec![Event::NewRed {
            suite: "test-b.sh".into(),
            last_green_round: "184".into(),
            last_green_commit: "c184".into(),
            first_red_round: "185".into(),
            first_red_commit: "c185".into()
        }]
    );
}

#[test]
fn a_red_event_is_raised_once_and_not_again_while_the_suite_stays_red() {
    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "seed");
    assert_eq!(run(&mut h, &[o("test-a.sh", Verdict::Red)], "c2", "2", "full").len(), 1);
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Red)], "c3", "3", "full").is_empty());
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c4", "4", "full").is_empty());
    let again = run(&mut h, &[o("test-a.sh", Verdict::Red)], "c5", "5", "full");
    assert!(matches!(&again[0], Event::NewRed { last_green_round, .. } if last_green_round == "4"));
}

#[test]
fn a_fault_is_its_own_event_and_never_moves_the_window() {
    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "seed");
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Fault)], "c2", "2", "full");
    assert!(matches!(ev.as_slice(), [Event::Fault { round, .. }] if round == "2"));
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Red)], "c3", "3", "full");
    assert!(matches!(ev.as_slice(), [Event::NewRed { last_green_round, first_red_round, .. }] if last_green_round == "1" && first_red_round == "3"));
}

#[test]
fn a_fault_after_a_red_does_not_look_like_a_recovery() {
    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "seed");
    run(&mut h, &[o("test-a.sh", Verdict::Red)], "c2", "2", "full");
    run(&mut h, &[o("test-a.sh", Verdict::Fault)], "c3", "3", "full");
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Red)], "c4", "4", "full").is_empty());
}

#[test]
fn skips_are_recorded_and_say_nothing() {
    let mut h = Vec::new();
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Skip)], "c1", "1", "subset");
    assert!(ev.is_empty());
    assert_eq!(h.len(), 1);
}

#[test]
fn red_and_green_on_one_commit_is_one_flip_in_either_order() {
    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "full");
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Red)], "c1", "1", "subset");
    assert!(matches!(ev.as_slice(), [Event::Flip { commit, .. }] if commit == "c1"));
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Red)], "c1", "1", "subset").is_empty());
    assert!(run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "subset").is_empty());

    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Red)], "c1", "1", "full");
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c1", "1", "subset");
    assert!(matches!(ev.as_slice(), [Event::Flip { .. }]));
}

#[test]
fn a_red_with_no_green_anywhere_names_no_window() {
    let mut h = Vec::new();
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Red)], "c1", "1", "full");
    assert!(matches!(ev.as_slice(), [Event::RedUnbounded { .. }]));
}

#[test]
fn a_seeded_green_round_gives_the_first_red_a_last_green() {
    let mut h = Vec::new();
    run(&mut h, &[o("test-a.sh", Verdict::Ok)], "c183", "183", "seed");
    let ev = run(&mut h, &[o("test-a.sh", Verdict::Red)], "c190", "190", "subset");
    assert!(matches!(ev.as_slice(), [Event::NewRed { last_green_round, .. }] if last_green_round == "183"));
}

#[test]
fn testenv_status_words_fold_to_four_states() {
    for (w, v) in [
        ("ok", Verdict::Ok), ("RED", Verdict::Red), ("timeout", Verdict::Red), ("quarantined-red", Verdict::Red),
        ("FAULT", Verdict::Fault), ("skip", Verdict::Skip), ("SKIPPED", Verdict::Skip), ("deferred", Verdict::Skip), ("skip-req", Verdict::Skip),
    ] {
        assert_eq!(Verdict::from_status(w), Some(v), "{w}");
    }
    assert_eq!(Verdict::from_status("wat"), None);
}

#[test]
fn stdout_parser_keeps_fault_and_skip_lines_it_once_dropped() {
    let text = "== batch ==\n  test-a.sh                        ok      7s\n  test-b.sh                        RED     rc=1 after 3s\n  test-c.sh                        FAULT   podman lost the exit status after 246s (rc=255) — not a suite defect\n  test-d.sh                        SKIPPED\n  test-e.sh                        SKIP-REQ requires:jq\nVERDICT RED\n";
    let r = parse_testenv_stdout(text);
    let got: Vec<(&str, Verdict, Option<u64>)> = r.iter().map(|x| (x.suite.as_str(), x.verdict, x.secs)).collect();
    assert_eq!(
        got,
        vec![
            ("test-a.sh", Verdict::Ok, Some(7)),
            ("test-b.sh", Verdict::Red, Some(3)),
            ("test-c.sh", Verdict::Fault, Some(246)),
            ("test-d.sh", Verdict::Skip, None),
            ("test-e.sh", Verdict::Skip, None),
        ]
    );
}

#[test]
fn result_file_parser_reads_status_and_seconds() {
    let r = parse_result_file("test-a.sh", "red 1 2 fp p e 1\n").unwrap();
    assert_eq!((r.verdict, r.secs), (Verdict::Red, Some(2)));
    assert_eq!(parse_result_file("t", "fault 1 246 - p e 0").unwrap().verdict, Verdict::Fault);
    assert!(parse_result_file("t", "").is_none());
}

#[test]
fn row_round_trips_through_its_tsd_json() {
    let row = Row { commit: "abc".into(), round: "9".into(), suite: "test-a.sh".into(), verdict: Verdict::Fault, secs: Some(4), mode: "full".into() };
    let line = tsd::build_row("2026-10-02T00:00:00Z", "h", FAMILY, &row.fields()).unwrap();
    assert_eq!(Row::from_json(&line), Some(row));
    assert_eq!(Row::from_json("not json"), None);
}

#[test]
fn subset_is_a_fraction_resampled_by_seed() {
    let all: Vec<String> = (0..40).map(|i| format!("test-{i:02}.sh")).collect();
    let a = pick_subset(&all, 4, 1);
    assert_eq!(a.len(), 10);
    assert_eq!(a, pick_subset(&all, 4, 1));
    assert_ne!(a, pick_subset(&all, 4, 2));
    assert_eq!(pick_subset(&all[..2], 4, 1).len(), 1);
    assert!(pick_subset(&[], 4, 1).is_empty());
}

#[test]
fn bisect_finds_the_first_red_and_gives_up_on_a_probe_without_a_verdict() {
    for first in 0..7 {
        assert_eq!(bisect(7, |i| Some(i >= first)), Some(first));
    }
    assert_eq!(bisect(7, |_| None), None);
    assert_eq!(bisect(0, |_| Some(true)), None);
}

#[test]
fn judge_calls_any_green_rerun_a_flip_and_no_verdict_inconclusive() {
    assert_eq!(judge(&[Verdict::Red, Verdict::Ok, Verdict::Red]), Judgement::Flaky);
    assert_eq!(judge(&[Verdict::Red, Verdict::Red]), Judgement::Reproducible);
    assert_eq!(judge(&[Verdict::Fault, Verdict::Skip]), Judgement::Inconclusive);
}

#[test]
fn members_name_their_bead_only_when_the_subject_is_a_merge() {
    let m = parse_members("aaa\tround-q: merge sp-x1 (b1)\nbbb\ttweak things\n");
    assert_eq!(m[0].bead.as_deref(), Some("sp-x1"));
    assert_eq!(m[1], Member { commit: "bbb".into(), bead: None });
}

/// sp-mve9i: a filed bug is still open while its lifecycle row is not terminal — bd's status
/// is not consulted, and a bead with no row is not open.
#[test]
fn a_bug_is_open_while_its_lifecycle_row_is_not_terminal() {
    use spira_config::lc_state::Row;
    let row = |id: &str, st: &str| (id.to_string(), Row { bead_id: id.into(), state: st.into(), ..Default::default() });
    let lc: std::collections::HashMap<String, Row> = [row("sp-w", "WORKING"), row("sp-s", "SUBMITTED"), row("sp-l", "LANDED"), row("sp-d", "DROPPED")].into_iter().collect();
    let all: Vec<(String, String)> = ["sp-w", "sp-s", "sp-l", "sp-d", "sp-none"].iter().map(|i| (i.to_string(), format!("t {i}"))).collect();
    let ids: Vec<String> = still_open(all, &lc).into_iter().map(|(i, _)| i).collect();
    assert_eq!(ids, vec!["sp-w", "sp-s"]);
}

#[test]
fn an_open_bead_only_covers_its_own_suite_and_kind() {
    let open = vec![("sp-1".to_string(), "cert-sweep: test-a.sh flips on one commit".to_string())];
    assert_eq!(open_duplicate(&open, "test-a.sh", FilingKind::Flip), Some("sp-1"));
    assert_eq!(open_duplicate(&open, "test-a.sh", FilingKind::Red), None);
    assert_eq!(open_duplicate(&open, "test-b.sh", FilingKind::Flip), None);
}

#[test]
fn the_first_fail_line_is_the_first_one() {
    assert_eq!(first_fail_line("ok\n  FAIL: one\nFAIL: two\n").as_deref(), Some("FAIL: one"));
    assert_eq!(first_fail_line("nothing"), None);
}

#[test]
fn a_runner_verdict_line_is_found_and_absence_is_none() {
    let out = "building\nVERDICT FAULT rc=2 ran=0 reason=deadline-build\n";
    assert_eq!(runner_verdict(out), Some("VERDICT FAULT rc=2 ran=0 reason=deadline-build"));
    assert_eq!(runner_verdict("nothing here\n"), None);
}
