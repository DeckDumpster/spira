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

/// How many timer periods a sentinel pass may run before its own wall time is itself a flow
/// gap (design reconciler-time-series-2026-09-27 §3: "pass wall time more than twice the
/// timer period").
const SENTINEL_OVERRUN_RATIO: f64 = 2.0;

/// A stage's p90 dwell compared to this multiple of its own trailing baseline p90 — a
/// coarser, longer-horizon regression signal than `dwell_raw`'s (which compares a p95 to a
/// configured limit, falling back to a 2x baseline ratio only when no limit is set). This one
/// has no configured limit at all, only ever its own history.
const DWELL_REGRESSION_RATIO: f64 = 3.0;

/// Reopens per landed bead above which the rate is a gap — one reopen per landed bead, taken
/// as-is rather than derived from a fixture. The caller (reconciler-flow's main) holds this
/// report-only until 24h of `bead-stage` history exist.
const REWORK_THRESHOLD: f64 = 1.0;

/// The lifecycle states `stage_dwell_regression_raw` is evaluated for every pass — the same
/// list design row 116 names for the existing per-stage dwell invariant, so a bead sitting in
/// a terminal state (LANDED, DONE, SUPERSEDED, DROPPED) is never asked "how much longer than
/// usual", which has no meaning once nothing follows.
pub const DWELL_REGRESSION_STATES: [&str; 5] = ["READY", "WORKING", "SUBMITTED", "IN_DELIVERY", "REWORK"];

/// Current vs. trailing-baseline backlog size (a self-produced tsd series — see
/// `io::backlog_count` and the `backlog` family it appends to every pass).
pub struct BacklogObserved {
    pub current: u64,
    pub baseline: f64,
}

/// "The backlog drains" (the design's intent): a backlog trending
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

/// One `slots` family sample (see `io::slots_samples`): the fleet's live/ceiling capacity,
/// how much work is ready to claim, and whether admission is deliberately throttled right
/// now — a paused fleet's idle slots are a decision, not a gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotsSample {
    pub live: u64,
    pub ceiling: u64,
    pub ready: u64,
    pub capacity_paused: bool,
}

fn slot_is_idle(s: &SlotsSample) -> bool {
    !s.capacity_paused && s.ready > 0 && s.live < s.ceiling
}

/// "A free slot while ready work exists, across two `slots` samples a pass apart" (design
/// §3): a single blip must not fire this — a fleet in the middle of turning a slot over can
/// show one idle sample and be fine a moment later, so only two consecutive idle readings are
/// a gap.
pub fn idle_capacity_raw(prev: &SlotsSample, current: &SlotsSample) -> RawStatus {
    if slot_is_idle(prev) && slot_is_idle(current) {
        RawStatus::Gap {
            desired: format!("live == ceiling ({}) while ready work waits, or ready == 0", current.ceiling),
            observed: format!(
                "live {} < ceiling {} with {} ready, on two consecutive samples",
                current.live, current.ceiling, current.ready
            ),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// "Sentinel overrun: pass wall time more than twice the timer period" (design §3) — only a
/// pass running past double the period fires.
pub fn sentinel_overrun_raw(pass_wall_secs: u64, timer_period_secs: u64) -> RawStatus {
    let ceiling = timer_period_secs as f64 * SENTINEL_OVERRUN_RATIO;
    if (pass_wall_secs as f64) > ceiling {
        RawStatus::Gap {
            desired: format!("<= {ceiling:.0}s ({SENTINEL_OVERRUN_RATIO:.0}x the {timer_period_secs}s timer period)"),
            observed: format!("{pass_wall_secs}s"),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// Reopens (transitions to REWORK) and beads landed, both over the rework window (design: 6h,
/// distinct from the 30-minute flow window the other new invariants share).
pub struct ReworkObserved {
    pub reopens: u64,
    pub landed: u64,
}

/// A landed bead with no reopens at all is always satisfied, even before any bead has landed
/// in the window — `landed == 0` alone is not a gap, since there is no denominator to judge a
/// rate against yet. But a reopen with nothing landing in the same window (an all-rework
/// window) is never satisfied: the ratio is not merely high, it is undefined in the healthy
/// direction, so it is treated as the worst case rather than skipped.
pub fn rework_raw(o: &ReworkObserved) -> RawStatus {
    if o.reopens == 0 {
        return RawStatus::Satisfied;
    }
    if o.landed == 0 {
        return RawStatus::Gap {
            desired: format!("<= {REWORK_THRESHOLD:.1} reopens per landed bead"),
            observed: format!("{} reopens, 0 landed", o.reopens),
            since_hint: None,
        };
    }
    let rate = o.reopens as f64 / o.landed as f64;
    if rate > REWORK_THRESHOLD {
        RawStatus::Gap {
            desired: format!("<= {REWORK_THRESHOLD:.1} reopens per landed bead"),
            observed: format!("{rate:.2} ({} reopens / {} landed)", o.reopens, o.landed),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    }
}

/// One lifecycle state's p90 dwell (seconds) for the current window and its trailing
/// baseline, plus how many completed transitions (a bead entering the state and then leaving
/// it) each is drawn from.
pub struct DwellRegressionObserved {
    pub p90_seconds: f64,
    pub n: u64,
    pub baseline_p90_seconds: f64,
    pub baseline_n: u64,
}

/// "Stage dwell regression: p90 more than 3x its trailing baseline" (design §3) — unlike
/// `dwell_raw`'s configured-limit case, this has no floor to fall back on: no baseline yet
/// (or no transitions this pass) is satisfied, since there is nothing yet to have regressed
/// against.
pub fn dwell_regression_raw(o: &DwellRegressionObserved) -> RawStatus {
    if o.n == 0 || o.baseline_n == 0 || o.baseline_p90_seconds <= 0.0 {
        return RawStatus::Satisfied;
    }
    let ceiling = o.baseline_p90_seconds * DWELL_REGRESSION_RATIO;
    if o.p90_seconds > ceiling {
        RawStatus::Gap {
            desired: format!(
                "p90 <= {ceiling:.0}s ({DWELL_REGRESSION_RATIO:.0}x the trailing baseline p90 {:.0}s)",
                o.baseline_p90_seconds
            ),
            observed: format!("p90 {:.0}s over {} transitions", o.p90_seconds, o.n),
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

    // ── idle capacity ───────────────────────────────────────────────────────────────────

    fn busy() -> SlotsSample {
        SlotsSample { live: 8, ceiling: 8, ready: 12, capacity_paused: false }
    }

    fn idle() -> SlotsSample {
        SlotsSample { live: 3, ceiling: 8, ready: 12, capacity_paused: false }
    }

    #[test]
    fn idle_capacity_satisfied_when_full() {
        let s = idle_capacity_raw(&busy(), &busy());
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn idle_capacity_satisfied_on_a_single_idle_sample() {
        // A blip: idle on the most recent sample only must not fire.
        let s = idle_capacity_raw(&busy(), &idle());
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn idle_capacity_gap_when_idle_across_two_samples() {
        // Positive control: the literal case the invariant exists to catch.
        let s = idle_capacity_raw(&idle(), &idle());
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn idle_capacity_satisfied_with_nothing_ready() {
        let empty_queue = SlotsSample { ready: 0, ..idle() };
        let s = idle_capacity_raw(&empty_queue, &empty_queue);
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn idle_capacity_satisfied_while_capacity_is_deliberately_paused() {
        let paused = SlotsSample { capacity_paused: true, ..idle() };
        let s = idle_capacity_raw(&paused, &paused);
        assert_eq!(s, RawStatus::Satisfied);
    }

    // ── sentinel overrun ────────────────────────────────────────────────────────────────

    #[test]
    fn sentinel_overrun_satisfied_at_the_median_recon_finding() {
        // The literal recon number (213s against a 2-minute/120s timer) is over one period
        // but not two — this invariant's whole point is not to fire on that.
        let s = sentinel_overrun_raw(213, 120);
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn sentinel_overrun_gap_past_twice_the_period() {
        let s = sentinel_overrun_raw(241, 120);
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn sentinel_overrun_satisfied_at_exactly_twice_the_period() {
        let s = sentinel_overrun_raw(240, 120);
        assert_eq!(s, RawStatus::Satisfied);
    }

    // ── rework ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn rework_satisfied_with_no_reopens() {
        let s = rework_raw(&ReworkObserved { reopens: 0, landed: 0 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn rework_satisfied_at_or_under_one_reopen_per_landed() {
        let s = rework_raw(&ReworkObserved { reopens: 4, landed: 4 });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn rework_gap_over_one_reopen_per_landed() {
        // Positive control: the design's own default threshold, crossed.
        let s = rework_raw(&ReworkObserved { reopens: 5, landed: 4 });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    #[test]
    fn rework_gap_when_reopens_exist_and_nothing_landed() {
        let s = rework_raw(&ReworkObserved { reopens: 2, landed: 0 });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }

    // ── stage dwell regression ──────────────────────────────────────────────────────────

    #[test]
    fn dwell_regression_satisfied_with_no_baseline_yet() {
        let s = dwell_regression_raw(&DwellRegressionObserved {
            p90_seconds: 99999.0,
            n: 10,
            baseline_p90_seconds: 0.0,
            baseline_n: 0,
        });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn dwell_regression_satisfied_with_no_transitions_this_window() {
        let s = dwell_regression_raw(&DwellRegressionObserved {
            p90_seconds: 0.0,
            n: 0,
            baseline_p90_seconds: 300.0,
            baseline_n: 20,
        });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn dwell_regression_satisfied_within_three_times_baseline() {
        let s = dwell_regression_raw(&DwellRegressionObserved {
            p90_seconds: 800.0,
            n: 10,
            baseline_p90_seconds: 300.0,
            baseline_n: 20,
        });
        assert_eq!(s, RawStatus::Satisfied);
    }

    #[test]
    fn dwell_regression_gap_past_three_times_baseline() {
        // Positive control: the design's own ratio, crossed.
        let s = dwell_regression_raw(&DwellRegressionObserved {
            p90_seconds: 901.0,
            n: 10,
            baseline_p90_seconds: 300.0,
            baseline_n: 20,
        });
        assert!(matches!(s, RawStatus::Gap { .. }), "expected a gap, got {s:?}");
    }
}
