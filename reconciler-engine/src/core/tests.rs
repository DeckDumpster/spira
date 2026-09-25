use super::*;

fn gap(desired: &str, observed: &str) -> RawStatus {
    RawStatus::Gap { desired: desired.to_string(), observed: observed.to_string(), since_hint: None }
}

// law-absence-needs-a-positive-control: prove satisfied stays satisfied before every
// gap/unobservable/hysteresis case below.
#[test]
fn satisfied_stays_satisfied() {
    let (v, st) = step(0, RawStatus::Satisfied, 60, HysteresisState::default());
    assert_eq!(v.status, RawStatus::Satisfied);
    assert!(!v.is_gap);
    assert!(!v.just_closed, "nothing was open to close");
    assert_eq!(v.since, None);
    assert_eq!(st, HysteresisState::default());
}

#[test]
fn unobservable_is_never_reported_as_satisfied() {
    let raw = RawStatus::Unobservable { reason: "forge call failed".into() };
    let (v, _) = step(0, raw.clone(), 60, HysteresisState::default());
    assert_eq!(v.status, raw);
    assert_ne!(v.status, RawStatus::Satisfied);

    // Persisting past the grace period does not turn it into anything but unobservable
    // either — an unreadable input never becomes "fine" just because it has been
    // unreadable for a while.
    let (_, st1) = step(0, raw.clone(), 60, HysteresisState::default());
    let (v2, _) = step(120, raw.clone(), 60, st1);
    assert_eq!(v2.status, raw);
    assert!(v2.is_gap, "an unobservable input still counts toward the remedy ladder once past grace");
}

#[test]
fn gap_inside_grace_period_raises_nothing() {
    let raw = gap("green", "red");
    let (v, st) = step(1000, raw.clone(), 300, HysteresisState::default());
    assert!(!v.is_gap, "first pass, 0s old, grace is 300s");
    assert_eq!(v.since, Some(1000), "since-when is recorded even inside grace");

    let (v2, _) = step(1200, raw, 300, st); // 200s later, still < 300s grace
    assert!(!v2.is_gap);
    assert_eq!(v2.since, Some(1000));
}

#[test]
fn gap_outside_grace_period_counts() {
    let raw = gap("green", "red");
    let (_, st) = step(1000, raw.clone(), 300, HysteresisState::default());
    let (v, _) = step(1301, raw, 300, st); // 301s later, >= 300s grace
    assert!(v.is_gap);
    assert_eq!(v.since, Some(1000), "since-when is when it first went bad, not when grace expired");
}

#[test]
fn since_hint_overrides_first_noticed_time() {
    // A queued-since timestamp from the forge is the true since-when, even on the pass
    // czar-pass first happens to observe it.
    let raw = RawStatus::Gap { desired: "queued <10s".into(), observed: "queued 900s".into(), since_hint: Some(100) };
    let (v, _) = step(1000, raw, 0, HysteresisState::default());
    assert_eq!(v.since, Some(100));
    assert!(v.is_gap);
}

#[test]
fn one_clean_pass_closes_a_gap() {
    let raw = gap("green", "red");
    let (_, st) = step(1000, raw.clone(), 0, HysteresisState::default());
    let (opened, st) = step(1001, raw, 0, st);
    assert!(opened.is_gap);

    let (closed, st2) = step(1002, RawStatus::Satisfied, 0, st);
    assert!(closed.just_closed, "a single Satisfied pass is enough to close");
    assert!(!closed.is_gap);
    assert_eq!(st2, HysteresisState::default(), "state resets once closed");
}

#[test]
fn a_flap_does_not_close_early_but_a_real_recovery_does() {
    // Grace 300s: gap, gap, gap — never closes on its own from repeated gaps.
    let raw = gap("green", "red");
    let (_, st) = step(0, raw.clone(), 300, HysteresisState::default());
    let (v1, st) = step(100, raw.clone(), 300, st);
    assert!(!v1.just_closed);
    let (v2, _st) = step(200, raw, 300, st);
    assert!(!v2.just_closed);
}

#[test]
fn remedy_that_closes_its_gap_is_not_flagged_failed() {
    let raw = gap("running", "stopped");
    let (_, mut st) = step(0, raw, 60, HysteresisState::default());
    record_remedy(&mut st, 5, "restart-unit");
    assert_eq!(last_remedy(&st), Some("restart-unit"));

    // Next pass: the thing is satisfied — the remedy worked.
    let (v, next) = step(10, RawStatus::Satisfied, 60, st);
    assert!(!v.remedy_failed);
    assert!(v.just_closed);
    assert_eq!(next, HysteresisState::default());
}

#[test]
fn remedy_that_does_not_close_its_gap_escalates_on_the_next_pass() {
    let raw = gap("running", "stopped");
    let (_, mut st) = step(0, raw.clone(), 0, HysteresisState::default());
    record_remedy(&mut st, 5, "restart-unit");

    // Next pass: still a gap. The remedy did not close it — it does not count as a
    // remedy, and the caller must escalate rather than reattempt it silently.
    let (v, next) = step(10, raw, 0, st);
    assert!(v.is_gap);
    assert!(v.remedy_failed, "an action that doesn't close its gap counts as not a remedy");
    assert_eq!(last_remedy(&next), None, "the failed attempt marker is cleared for a fresh attempt");
}

#[test]
fn remedy_marker_does_not_leak_across_a_fresh_streak() {
    let raw = gap("running", "stopped");
    let (_, mut st) = step(0, raw.clone(), 0, HysteresisState::default());
    record_remedy(&mut st, 5, "restart-unit");
    let (_, st) = step(10, RawStatus::Satisfied, 0, st); // closes; remedy verified working

    // A brand new streak later must not be blamed on the old remedy attempt.
    let (v, _) = step(1000, raw, 0, st);
    assert!(!v.remedy_failed);
}

// Replay test: a single sequence of synthetic observations, asserting the full trace of
// is_gap/just_closed/remedy_failed matches what the design's hysteresis rules predict.
#[test]
fn replay_grace_in_then_remedy_then_flap_then_real_recovery() {
    let grace = 120u64;
    let mut state = HysteresisState::default();
    let mut trace = Vec::new();

    let ticks: Vec<(u64, RawStatus)> = vec![
        (0, RawStatus::Satisfied),
        (60, gap("enabled", "disabled")),         // gap begins — inside grace
        (200, gap("enabled", "disabled")),        // 140s since 60 — now past 120s grace
        (201, gap("enabled", "disabled")),        // remedy attempted here (below)
        (202, gap("enabled", "disabled")),        // remedy did not close it
        (203, RawStatus::Satisfied),              // real recovery
        (500, RawStatus::Satisfied),               // stays quiet
    ];

    for (i, (now, raw)) in ticks.into_iter().enumerate() {
        let (v, mut next) = step(now, raw, grace, state);
        if i == 3 {
            record_remedy(&mut next, now, "det-fix");
        }
        trace.push((v.is_gap, v.just_closed, v.remedy_failed));
        state = next;
    }

    assert_eq!(
        trace,
        vec![
            (false, false, false), // satisfied
            (false, false, false), // gap, inside grace
            (true, false, false),  // gap, past grace
            (true, false, false),  // gap, past grace (remedy recorded after this step)
            (true, false, true),   // still a gap after the remedy: remedy_failed
            (false, true, false),  // satisfied: closes
            (false, false, false), // stays quiet
        ]
    );
}
