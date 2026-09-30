//! Per-unit restart-count baselines — auron.sh's own arithmetic, as a pure function. A
//! count that went DOWN (daemon-reload, reinstall) resets the baseline, not an alert. A
//! window that expired slides the baseline forward so stale data does not keep a unit in
//! alert forever on one long-ago burst.

use crate::state::RestartBaseline;

/// `current`'s restart count observed this pass, against the prior baseline (`None` the
/// first time a unit is ever seen). Returns the updated baseline to persist and the delta
/// to compare against the alert threshold.
pub fn update(prior: Option<&RestartBaseline>, current: i64, now: i64, window_s: i64) -> (RestartBaseline, i64) {
    let mut base = prior.map(|p| p.baseline).unwrap_or(current);
    let mut base_at = prior.map(|p| p.baseline_at).unwrap_or(now);
    let last = prior.map(|p| p.last).unwrap_or(current);
    // Two distinct reasons to reset the baseline, same action for both: the count went
    // DOWN (daemon-reload, reinstall — not an alert), or the window simply expired (a
    // stale old burst must not keep a unit in alert forever).
    if current < last || now - base_at >= window_s {
        base = current;
        base_at = now;
    }
    (RestartBaseline { baseline: base, baseline_at: base_at, last: current }, current - base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_seen_for_the_first_time_has_zero_delta() {
        let (base, delta) = update(None, 7, 1000, 3600);
        assert_eq!(delta, 0, "first-run floor: restarts before Auron ever saw the unit do not count");
        assert_eq!(base.baseline, 7);
        assert_eq!(base.last, 7);
    }

    #[test]
    fn restart_count_below_threshold_produces_a_small_delta() {
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 2 };
        let (_, delta) = update(Some(&prior), 4, 1100, 3600);
        assert_eq!(delta, 2);
    }

    #[test]
    fn restart_count_above_threshold_produces_a_large_delta() {
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 2 };
        let (_, delta) = update(Some(&prior), 10, 1100, 3600);
        assert_eq!(delta, 8);
    }

    #[test]
    fn a_count_that_went_down_resets_the_baseline_not_an_alert() {
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 9 };
        let (base, delta) = update(Some(&prior), 0, 1100, 3600); // daemon-reload / reinstall
        assert_eq!(delta, 0);
        assert_eq!(base.baseline, 0);
        assert_eq!(base.baseline_at, 1100);
    }

    #[test]
    fn an_expired_window_slides_the_baseline_forward() {
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 8 };
        // 3600s later, count unchanged at 8: without the slide this would read delta=6
        // forever off one old burst.
        let (base, delta) = update(Some(&prior), 8, 1000 + 3600, 3600);
        assert_eq!(delta, 0);
        assert_eq!(base.baseline, 8);
        assert_eq!(base.baseline_at, 1000 + 3600);
    }

    #[test]
    fn a_window_not_yet_expired_keeps_the_old_baseline() {
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 8 };
        let (base, delta) = update(Some(&prior), 8, 1000 + 3599, 3600);
        assert_eq!(delta, 6);
        assert_eq!(base.baseline, 2);
    }

    #[test]
    fn a_second_pass_on_the_same_standing_burst_reports_the_same_delta() {
        // "restart: a second pass on the same standing loop does not raise a duplicate" —
        // the baseline does not move just because a pass ran; only a reset or a slide
        // moves it, so the dedup is the alert-bead layer's job (one bead per cause),
        // not this arithmetic's.
        let prior = RestartBaseline { baseline: 2, baseline_at: 1000, last: 10 };
        let (base1, d1) = update(Some(&prior), 10, 1050, 3600);
        let (base2, d2) = update(Some(&base1), 10, 1100, 3600);
        assert_eq!((d1, d2), (8, 8));
        assert_eq!(base1, base2);
    }

    #[test]
    fn count_rising_again_after_a_reset_produces_a_fresh_delta() {
        let prior = RestartBaseline { baseline: 0, baseline_at: 1100, last: 0 };
        let (_, delta) = update(Some(&prior), 6, 1150, 3600);
        assert_eq!(delta, 6);
    }
}
