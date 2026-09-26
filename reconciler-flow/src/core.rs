//! The pure half of the flow invariants: plain observed numbers and an optional desired
//! floor/limit in, a [`RawStatus`] out. No clock, no file, no subprocess — [`crate::io`] is
//! the only place any of those appear, so every branch here is a fixture a test can replay.
//!
//! Hysteresis (grace period, one-clean-pass-closes, unobservable-is-never-satisfied) is not
//! reimplemented here: every function returns a [`RawStatus`] that the caller feeds straight
//! into [`reconciler_engine::core::step`], the same engine czar-pass's structural invariants
//! use (sp-pu7v6). A flow gap has no deterministic remedy (per the design), so nothing here
//! ever proposes one — the caller's only action on `is_gap` is the Concierge alert path.

use reconciler_engine::core::RawStatus;

/// A ratio applied to the trailing baseline: current must be at least this fraction of it.
/// Below a floor is always a gap regardless of baseline; below this fraction of a healthy
/// baseline is a gap even with no floor configured, so a slowdown is visible before a
/// configured floor exists to catch it.
const BASELINE_RATIO: f64 = 0.5;

/// Growth beyond this multiple of the trailing 24h baseline backlog size is a gap. 1.25
/// tolerates ordinary day-to-day variance; the intent is "the backlog drains", not "the
/// backlog never has a bad afternoon".
const BACKLOG_GROWTH_RATIO: f64 = 1.25;

/// A stage's dwell is compared against this multiple of its own trailing baseline p95 when
/// no `dwell_limit_seconds` override names it — same reasoning as `BASELINE_RATIO`, mirrored
/// upward since dwell is "how long", not "how much".
const DWELL_BASELINE_RATIO: f64 = 2.0;

/// Below this many flips a rate is not evaluated at all — a rate computed from one or two
/// events swings wildly and is noise, not signal.
const ROUND_HEALTH_MIN_FLIPS: u64 = 2;

/// A flip rate above this absolute value is a gap even against a noisy or absent baseline —
/// thrash this frequent is never healthy regardless of history.
const ROUND_HEALTH_ABSOLUTE_FLOOR: f64 = 0.2;

/// Current vs. trailing-baseline backlog size (a self-produced tsd series — see
/// `io::backlog_count` and the `backlog` family it appends to every pass).
pub struct BacklogObserved {
    pub current: u64,
    pub baseline: f64,
}

/// "The backlog drains" (per the design's intent, verbatim from Ryan): a backlog trending
/// past `BACKLOG_GROWTH_RATIO` times its own trailing 24h average is the flow gap this
/// invariant exists to catch. A `baseline` of zero means there is no history yet — not a
/// gap, since there is nothing yet to have grown past.
pub fn backlog_trend_raw(o: &BacklogObserved) -> RawStatus {
    if o.baseline <= 0.0 {
        return RawStatus::Satisfied;
    }
    let ceiling = o.baseline * BACKLOG_GROWTH_RATIO;
    if (o.current as f64) > ceiling {
        RawStatus::Gap {
            desired: format!("<= {:.1} ({:.0}% of 24h baseline {:.1})", ceiling, BACKLOG_GROWTH_RATIO * 100.0, o.baseline),
            observed: o.current.to_string(),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// A stage's current rate (events/hour) against its own trailing baseline, plus how much
/// work is actually waiting on it. `waiting == 0` means nothing needs the stage to run right
/// now — a rate of zero with nothing waiting is a quiet system, not a stall (this is the
/// literal "a land rate of zero for 30 minutes with certified work waiting" test case: it is
/// `waiting` that turns a zero rate into a gap, not the rate alone).
pub struct VelocityObserved {
    pub current_per_hour: f64,
    pub baseline_per_hour: f64,
    pub waiting: u64,
}

pub fn velocity_raw(o: &VelocityObserved, floor_per_hour: Option<f64>) -> RawStatus {
    if o.waiting == 0 {
        return RawStatus::Satisfied;
    }
    let floor = floor_per_hour.unwrap_or(0.0);
    let baseline_floor = o.baseline_per_hour * BASELINE_RATIO;
    let threshold = floor.max(baseline_floor);
    if threshold <= 0.0 {
        // No floor configured and no baseline history yet: nothing to compare against.
        return RawStatus::Satisfied;
    }
    if o.current_per_hour < threshold {
        RawStatus::Gap {
            desired: format!(
                "> {threshold:.2}/h (floor {floor:.2}/h, {:.0}% of 24h baseline {:.2}/h)",
                BASELINE_RATIO * 100.0,
                o.baseline_per_hour
            ),
            observed: format!("{:.2}/h with {} waiting", o.current_per_hour, o.waiting),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// The p95 dwell time (seconds) a stage's items spent waiting, over the trailing window, and
/// how many completed transitions that p95 is drawn from. `n == 0` means nothing transitioned
/// in the window — there is no dwell to measure, which is not itself a gap.
pub struct DwellObserved {
    pub p95_seconds: f64,
    pub n: u64,
}

pub fn dwell_raw(o: &DwellObserved, limit_seconds: Option<u64>, baseline_p95_seconds: Option<f64>) -> RawStatus {
    if o.n == 0 {
        return RawStatus::Satisfied;
    }
    let mut threshold: Option<f64> = limit_seconds.map(|l| l as f64);
    if let Some(b) = baseline_p95_seconds {
        let baseline_threshold = b * DWELL_BASELINE_RATIO;
        threshold = Some(threshold.map_or(baseline_threshold, |t| t.min(baseline_threshold)));
    }
    let Some(threshold) = threshold else {
        // No configured limit and no baseline yet: nothing to compare against.
        return RawStatus::Satisfied;
    };
    if o.p95_seconds > threshold {
        RawStatus::Gap {
            desired: format!("p95 <= {threshold:.0}s"),
            observed: format!("p95 {:.0}s over {} transitions", o.p95_seconds, o.n),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// How often work reverses within the trailing window: `flips` is the count of transitions
/// back to RED after having reached CERTIFIED or LANDED — work that looked done and was not.
/// `transitions` is every transition in the same window, the denominator a rate needs.
pub struct RoundHealthObserved {
    pub flips: u64,
    pub transitions: u64,
    pub baseline_flip_rate: f64,
}

pub fn round_health_raw(o: &RoundHealthObserved) -> RawStatus {
    if o.transitions == 0 || o.flips < ROUND_HEALTH_MIN_FLIPS {
        return RawStatus::Satisfied;
    }
    let rate = o.flips as f64 / o.transitions as f64;
    let threshold = (o.baseline_flip_rate * DWELL_BASELINE_RATIO).max(ROUND_HEALTH_ABSOLUTE_FLOOR);
    if rate > threshold {
        RawStatus::Gap {
            desired: format!("flip rate <= {threshold:.2}"),
            observed: format!("{rate:.2} ({} of {} transitions)", o.flips, o.transitions),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── backlog trend ──────────────────────────────────────────────────────────────────

    #[test]
    fn backlog_trend_satisfied_with_no_baseline_history() {
        let s = backlog_trend_raw(&BacklogObserved { current: 500, baseline: 0.0 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn backlog_trend_satisfied_within_growth_tolerance() {
        let s = backlog_trend_raw(&BacklogObserved { current: 120, baseline: 100.0 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn backlog_trend_gap_when_growth_exceeds_tolerance() {
        // Positive control: a backlog well past the ratio must be caught, not waved through.
        let s = backlog_trend_raw(&BacklogObserved { current: 200, baseline: 100.0 });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    // ── velocity ────────────────────────────────────────────────────────────────────────

    #[test]
    fn velocity_satisfied_with_nothing_waiting() {
        // The literal non-goal: a zero rate is fine when there is nothing to do.
        let s = velocity_raw(
            &VelocityObserved { current_per_hour: 0.0, baseline_per_hour: 4.0, waiting: 0 },
            None,
        );
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn velocity_gap_zero_rate_with_certified_work_waiting() {
        // The design's own test case, verbatim: "a land rate of zero for 30 minutes with
        // certified work waiting".
        let s = velocity_raw(
            &VelocityObserved { current_per_hour: 0.0, baseline_per_hour: 4.0, waiting: 3 },
            None,
        );
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn velocity_satisfied_at_healthy_rate_with_work_waiting() {
        let s = velocity_raw(
            &VelocityObserved { current_per_hour: 3.5, baseline_per_hour: 4.0, waiting: 3 },
            None,
        );
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn velocity_gap_below_configured_floor_even_with_healthy_baseline_ratio() {
        // Baseline is tiny (so BASELINE_RATIO alone would not fire) but an explicit floor
        // from the desired document still applies.
        let s = velocity_raw(
            &VelocityObserved { current_per_hour: 0.5, baseline_per_hour: 0.6, waiting: 1 },
            Some(1.0),
        );
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn velocity_satisfied_with_no_floor_and_no_baseline_yet() {
        let s = velocity_raw(
            &VelocityObserved { current_per_hour: 0.0, baseline_per_hour: 0.0, waiting: 5 },
            None,
        );
        assert_eq!(s, RawStatus::Satisfied);
    }

    // ── dwell ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn dwell_satisfied_with_no_transitions() {
        let s = dwell_raw(&DwellObserved { p95_seconds: 99999.0, n: 0 }, Some(3600), None);
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn dwell_gap_over_configured_limit() {
        let s = dwell_raw(&DwellObserved { p95_seconds: 4000.0, n: 10 }, Some(3600), None);
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn dwell_satisfied_under_configured_limit() {
        let s = dwell_raw(&DwellObserved { p95_seconds: 1000.0, n: 10 }, Some(3600), None);
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn dwell_gap_over_baseline_ratio_with_no_limit_configured() {
        let s = dwell_raw(&DwellObserved { p95_seconds: 500.0, n: 10 }, None, Some(200.0));
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn dwell_satisfied_with_nothing_configured_and_no_baseline() {
        let s = dwell_raw(&DwellObserved { p95_seconds: 999999.0, n: 10 }, None, None);
        assert_eq!(s, RawStatus::Satisfied);
    }

    // ── round health ────────────────────────────────────────────────────────────────────

    #[test]
    fn round_health_satisfied_with_no_transitions() {
        let s = round_health_raw(&RoundHealthObserved { flips: 0, transitions: 0, baseline_flip_rate: 0.05 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn round_health_satisfied_below_min_flips_regardless_of_rate() {
        // 1 flip of 1 transition is a 100% rate but too small a sample to mean anything.
        let s = round_health_raw(&RoundHealthObserved { flips: 1, transitions: 1, baseline_flip_rate: 0.0 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn round_health_gap_when_thrashing_well_above_baseline() {
        let s = round_health_raw(&RoundHealthObserved { flips: 10, transitions: 20, baseline_flip_rate: 0.05 });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn round_health_satisfied_near_its_own_baseline() {
        let s = round_health_raw(&RoundHealthObserved { flips: 2, transitions: 40, baseline_flip_rate: 0.05 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn round_health_gap_on_absolute_floor_with_a_near_zero_baseline() {
        // A near-zero baseline scaled by the ratio is still near zero, so without an
        // absolute floor this rate would pass by comparison to a baseline with no real
        // signal in it.
        let s = round_health_raw(&RoundHealthObserved { flips: 5, transitions: 10, baseline_flip_rate: 0.0 });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }
}
