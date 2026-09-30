//! Unit tests over `engine.rs`'s pure decision surface (DESIGN.md "Test strategy"). No
//! filesystem, no `/proc`, no live process — every input here is a hand-built `Snapshot` or a
//! plain value, so these are the tests that survive without a gate, a repo, or a host.

use super::*;

// --------------------------------------------------------------------------------- slug()

#[test]
fn slug_folds_unsafe_characters() {
    assert_eq!(slug("spira", "spira/sp-abc12"), "spira.spira_sp-abc12");
}

#[test]
fn slug_keeps_dots_underscores_dashes() {
    assert_eq!(slug("my.repo_1", "feature-x"), "my.repo_1.feature-x");
}

#[test]
fn slug_is_stable_for_the_same_pair() {
    assert_eq!(slug("spira", "spira/sp-1"), slug("spira", "spira/sp-1"));
}

// ---------------------------------------------------------------------------------- Key

#[test]
fn key_format_matches_bash_shape() {
    let k = Key::new("deadbeef", Some("cafefeed".into()));
    assert_eq!(k.format(), "deadbeef cafefeed");
}

#[test]
fn key_defaults_base_to_dash_when_landref_unresolved() {
    let k = Key::new("deadbeef", None);
    assert_eq!(k.format(), "deadbeef -");
}

#[test]
fn key_is_stale_when_recorded_text_differs() {
    let k = Key::new("newtip", Some("newbase".into()));
    assert!(k.is_stale("oldtip oldbase"));
    assert!(!k.is_stale("newtip newbase"));
}

// ----------------------------------------------------------------------- is_managed_alive()

#[test]
fn alive_requires_pid_proc_and_matching_argv() {
    assert!(is_managed_alive(Some(123), true, Some("/usr/bin/gate-run --exec spira/sp-1 spira")));
    assert!(!is_managed_alive(None, true, Some("gate-run --exec")));
    assert!(!is_managed_alive(Some(123), false, Some("gate-run --exec")));
    assert!(!is_managed_alive(Some(123), true, None));
    // A recycled pid running an unrelated program must never read as live.
    assert!(!is_managed_alive(Some(123), true, Some("/usr/bin/some-other-daemon")));
}

// ------------------------------------------------------------------------ find_unmanaged()

#[test]
fn unmanaged_matches_gate_sh_and_exact_branch() {
    let procs = vec![
        (10, "bash /home/spira/spira/gate.sh spira/sp-1 spira".to_string()),
        (20, "bash /home/spira/spira/gate.sh spira/sp-12 spira".to_string()),
    ];
    assert_eq!(find_unmanaged("spira/sp-1", 999, &procs), Some(10));
}

#[test]
fn unmanaged_excludes_self_and_non_gate_processes() {
    let procs = vec![
        (10, "bash /home/spira/spira/gate.sh spira/sp-1 spira".to_string()),
        (30, "sleep 10".to_string()),
    ];
    assert_eq!(find_unmanaged("spira/sp-1", 10, &procs), None);
    assert_eq!(find_unmanaged("spira/sp-9", 999, &procs), None);
}

// -------------------------------------------------------------------------- extract_suites()

#[test]
fn extract_suites_takes_first_match() {
    let out = "some noise\ngate: gate PASS covered suites: a.sh b.sh\nmore\ngate: gate PASS covered suites: c.sh\n";
    assert_eq!(extract_suites(out), "a.sh b.sh");
}

#[test]
fn extract_suites_defaults_to_dash() {
    assert_eq!(extract_suites("gate: gate FAIL\nsome output\n"), "-");
}

// ------------------------------------------------------------------------------- tail_n()

#[test]
fn tail_n_keeps_last_lines_only() {
    assert_eq!(tail_n("a\nb\nc\nd\n", 2), "c\nd");
}

#[test]
fn tail_n_shorter_than_n_keeps_everything() {
    assert_eq!(tail_n("a\nb\n", 5), "a\nb");
}

// ------------------------------------------------------------------------------- elapsed()

#[test]
fn elapsed_is_now_minus_started() {
    assert_eq!(elapsed(110, Some(100)), 10);
}

#[test]
fn elapsed_is_zero_without_a_start_marker() {
    // A missing start marker is its own case (an interrupted state directory), not a reason
    // to print an absurd wall-clock figure (DESIGN.md, mirroring the bash's own comment).
    assert_eq!(elapsed(100, None), 0);
}

// -------------------------------------------------------------------- decide_report() / render

fn snap(rc: Option<i32>, suites: Option<&str>, out: &str) -> Snapshot {
    Snapshot {
        dir_exists: true,
        managed_alive: false,
        pid: None,
        started: Some(0),
        recorded_key: None,
        rc,
        suites: suites.map(String::from),
        out_full: out.to_string(),
    }
}

#[test]
fn report_zero_is_passed() {
    let s = snap(Some(0), Some("a.sh b.sh"), "line1\nline2\n");
    let v = decide_report(&s);
    assert_eq!(v, Verdict::Passed { suites: "a.sh b.sh".into(), tail: "line1\nline2".into() });
    let (text, code) = render_verdict("spira/sp-1", "spira", 42, "tip base", &v);
    assert_eq!(code, 0);
    assert!(text.contains("gate-run: PASSED spira/sp-1 in spira after 42s\n"));
    assert!(text.contains("gate-run: key tip base\n"));
    // The exact prefix landing-pass::order::prior_pass_suites greps for.
    assert!(text.contains("gate-run: gate PASS covered suites: a.sh b.sh\n"));
}

#[test]
fn report_passed_defaults_suites_to_dash_when_file_absent() {
    let s = snap(Some(0), None, "");
    let v = decide_report(&s);
    assert_eq!(v, Verdict::Passed { suites: "-".into(), tail: String::new() });
}

#[test]
fn report_nonzero_is_failed_with_full_output() {
    let s = snap(Some(3), None, "FAIL: something broke\n");
    let v = decide_report(&s);
    assert_eq!(v, Verdict::Failed { rc: 3, out: "FAIL: something broke\n".into() });
    let (text, code) = render_verdict("spira/sp-1", "spira", 7, "tip base", &v);
    assert_eq!(code, 1);
    assert!(text.contains("gate-run: FAILED spira/sp-1 in spira after 7s (gate.sh exit 3)\n"));
    assert!(text.contains("FAIL: something broke\n"));
}

#[test]
fn report_missing_rc_is_died() {
    let s = snap(None, None, "partial output before it died\n");
    let v = decide_report(&s);
    assert_eq!(v, Verdict::Died);
    let (text, code) = render_verdict("spira/sp-1", "spira", 30, "tip base", &v);
    assert_eq!(code, 5);
    assert!(text.contains("gate-run: the gate run for spira/sp-1 died after 30s without recording a verdict\n"));
    // The died tail is separate diagnostic output (goes to stderr), not folded into the verdict.
    assert!(!text.contains("partial output"));
    assert_eq!(died_tail(&s), "partial output before it died");
}

// ------------------------------------------------------------------------- decide_status()

fn base_snapshot() -> Snapshot {
    Snapshot { dir_exists: false, managed_alive: false, pid: None, started: None, recorded_key: None, rc: None, suites: None, out_full: String::new() }
}

#[test]
fn status_no_gate_when_nothing_recorded() {
    let key = Key::new("tip", Some("base".into()));
    assert_eq!(decide_status(&key, &base_snapshot()), StatusOutcome::NoGate);
}

#[test]
fn status_still_running_when_alive() {
    let key = Key::new("tip", Some("base".into()));
    let s = Snapshot { managed_alive: true, pid: Some(42), recorded_key: Some(key.format()), ..base_snapshot() };
    assert_eq!(decide_status(&key, &s), StatusOutcome::StillRunning { stale: false });
}

#[test]
fn status_still_running_flags_stale_key_without_hiding_liveness() {
    // A live run for an EARLIER commit is still a live run (DESIGN.md): the answer is
    // "something is in flight, for the wrong tree", not "no gate ran".
    let key = Key::new("newtip", Some("newbase".into()));
    let s = Snapshot { managed_alive: true, pid: Some(42), recorded_key: Some("oldtip oldbase".into()), ..base_snapshot() };
    assert_eq!(decide_status(&key, &s), StatusOutcome::StillRunning { stale: true });
}

#[test]
fn status_verdict_when_finished_and_key_matches() {
    let key = Key::new("tip", Some("base".into()));
    let s = Snapshot { dir_exists: true, recorded_key: Some(key.format()), rc: Some(0), ..base_snapshot() };
    assert_eq!(decide_status(&key, &s), StatusOutcome::Verdict);
}

#[test]
fn status_stale_verdict_when_finished_for_a_different_tree() {
    let key = Key::new("newtip", Some("newbase".into()));
    let s = Snapshot { dir_exists: true, recorded_key: Some("oldtip oldbase".into()), rc: Some(0), ..base_snapshot() };
    assert_eq!(
        decide_status(&key, &s),
        StatusOutcome::StaleVerdict { recorded_key: "oldtip oldbase".into() }
    );
}

#[test]
fn status_verdict_not_stale_when_rc_absent_even_if_key_would_mismatch() {
    // Mirrors the bash: staleness is only checked once `rc` exists; a directory with a key
    // file but no rc yet (a dead, unrecorded run) falls through to report()'s own Died case
    // rather than being misreported as a rebase.
    let key = Key::new("newtip", Some("newbase".into()));
    let s = Snapshot { dir_exists: true, recorded_key: Some("oldtip oldbase".into()), rc: None, ..base_snapshot() };
    assert_eq!(decide_status(&key, &s), StatusOutcome::Verdict);
}

// ---------------------------------------------------------------------------- message text

#[test]
fn still_running_message_names_pid_and_elapsed() {
    let (text, code) = render_still_running("spira/sp-1", "tip base", 12, 4321, false);
    assert_eq!(code, 2);
    assert_eq!(text, "gate-run: still running for spira/sp-1 — 12s so far, pid 4321\ngate-run: key tip base\n");
}

#[test]
fn still_running_message_flags_earlier_commit() {
    let (text, _) = render_still_running("spira/sp-1", "tip base", 12, 4321, true);
    assert!(text.contains("(started for an earlier commit)"));
}

#[test]
fn stale_verdict_message_names_both_keys() {
    let (text, code) = render_stale_verdict("spira/sp-1", "old old", "new new");
    assert_eq!(code, 4);
    assert!(text.contains("key was old old, now new new"));
}

#[test]
fn no_gate_message_is_exit_three() {
    let (text, code) = render_no_gate("spira/sp-1", "tip base");
    assert_eq!(code, 3);
    assert!(text.contains("no gate has run or finished for spira/sp-1"));
}

#[test]
fn unmanaged_message_is_exit_two_and_names_pid() {
    let (text, code) = render_unmanaged("spira/sp-1", "tip base", 555);
    assert_eq!(code, 2);
    assert!(text.contains("pid 555"));
    assert!(text.contains("its verdict reaches nobody"));
}

#[test]
fn wait_timeout_message_is_exit_two() {
    let (text, code) = render_wait_timeout("spira/sp-1", 480, 480);
    assert_eq!(code, 2);
    assert!(text.contains("run the same command again"));
    assert!(text.contains("each call waits up to 480s"));
}
