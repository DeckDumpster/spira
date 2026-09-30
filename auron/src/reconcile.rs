//! The debounce state machine — auron.sh's own reconcile loop, as a pure function of the
//! firing set and the prior state. No bd call, no clock read, no file write: everything
//! here is `now`, thresholds, and two maps in, one decision per key out.
//!
//! Three defences against noise (auron.sh's own ordering, kept): a condition must hold for
//! `confirm` consecutive passes before it fires and clear for `clear` consecutive passes
//! before it is retracted, so nothing flaps on one sample; a standing alert's evidence is
//! rewritten at most every `refresh` seconds, not every pass; and one bead per cause
//! forever (flap-counted, reopened), never a second bead for the same cause.

use std::collections::{BTreeMap, BTreeSet};

use crate::state::KeyState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Nothing to do this pass.
    None,
    /// `state != firing`, now firing, confirmed: raise (create, or reopen-and-update if a
    /// prior bead for this key exists and is closed).
    ConfirmRaise,
    /// `state == firing`, no longer firing, confirmed clear: close the bead.
    ConfirmClear,
    /// `state == firing`, still firing, the refresh interval elapsed: rewrite the evidence
    /// — UNLESS the live bead is closed by hand, in which case the caller only bumps
    /// `refreshed` and never re-opens it (an operator's close is an acknowledgement, not
    /// a retraction Auron should fight).
    Refresh,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub key: String,
    /// The counters as bash's unconditional-before-the-branch updates left them: `seen`/
    /// `unseen` always; for `ConfirmRaise`, `first` and `flaps` are already bumped —
    /// tentatively, because a `flaps` bump rolls back if the write fails (bash's own
    /// asymmetry: `first` does not roll back; see `Step::rollback_failed_raise`).
    pub counters: KeyState,
    pub trigger: Trigger,
}

impl Step {
    /// The state to persist when the attempted write for this step's trigger succeeded.
    pub fn on_success(&self, now: i64) -> KeyState {
        let mut k = self.counters.clone();
        match self.trigger {
            Trigger::ConfirmRaise => {
                k.state = "firing".to_string();
                k.since = now;
                k.refreshed = now;
            }
            Trigger::ConfirmClear => {
                k.state = "clear".to_string();
                k.since = now;
            }
            Trigger::Refresh => {
                k.refreshed = now;
            }
            Trigger::None => {}
        }
        k
    }

    /// The state to persist when the attempted write failed (`beads_ok` goes false; the
    /// caller retries next pass). `ConfirmRaise` rolls back only the flap it tentatively
    /// added — `first` stays bumped, exactly auron.sh's own asymmetry.
    pub fn on_failure(&self) -> KeyState {
        let mut k = self.counters.clone();
        if self.trigger == Trigger::ConfirmRaise {
            k.flaps -= 1;
        }
        k
    }

    /// `Refresh` only, when the live bead is closed by hand: no write is even attempted —
    /// bump `refreshed` alone, so a hand-closed alert's evidence does not visibly rot but
    /// Auron never re-opens what the operator acknowledged.
    pub fn on_refresh_skip_closed(&self, now: i64) -> KeyState {
        let mut k = self.counters.clone();
        k.refreshed = now;
        k
    }
}

/// One step per key in `firing ∪ keys(prior)` — a key neither firing now nor known before
/// is never mentioned, exactly auron.sh's `all_keys`.
///
/// `db_reachable = false` updates `seen`/`unseen` (the one thing bash did before its own
/// `continue`) and stops there: no trigger, no `first`/`flaps` bump. THE DATABASE IS THE
/// CHANNEL, so with it down nothing below can be attempted; the counters still advance, so
/// the moment it answers again the transition happens on the next run rather than
/// starting over.
pub fn step_keys(now: i64, confirm: i64, clear_n: i64, refresh: i64, db_reachable: bool, firing: &BTreeSet<String>, prior: &BTreeMap<String, KeyState>) -> Vec<Step> {
    let mut all: BTreeSet<String> = firing.clone();
    all.extend(prior.keys().cloned());
    let mut steps = Vec::with_capacity(all.len());
    for k in all {
        let mut ks = prior.get(&k).cloned().unwrap_or_default();
        let is_firing = firing.contains(&k);
        if is_firing {
            ks.seen += 1;
            ks.unseen = 0;
        } else {
            ks.unseen += 1;
            ks.seen = 0;
        }
        let mut trigger = Trigger::None;
        if db_reachable {
            if ks.state != "firing" && is_firing && ks.seen >= confirm {
                ks.first = now;
                ks.flaps += 1;
                trigger = Trigger::ConfirmRaise;
            } else if ks.state == "firing" && !is_firing && ks.unseen >= clear_n {
                trigger = Trigger::ConfirmClear;
            } else if ks.state == "firing" && is_firing && now - ks.refreshed >= refresh {
                trigger = Trigger::Refresh;
            }
        }
        steps.push(Step { key: k, counters: ks, trigger });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ks: &[&str]) -> BTreeSet<String> {
        ks.iter().map(|s| s.to_string()).collect()
    }
    fn prior_with(k: &str, s: KeyState) -> BTreeMap<String, KeyState> {
        let mut m = BTreeMap::new();
        m.insert(k.to_string(), s);
        m
    }
    fn step_for<'a>(steps: &'a [Step], key: &str) -> &'a Step {
        steps.iter().find(|s| s.key == key).unwrap()
    }

    #[test]
    fn a_key_neither_firing_nor_known_never_appears() {
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&[]), &BTreeMap::new());
        assert!(steps.is_empty());
    }

    #[test]
    fn one_sighting_does_not_fire_a_condition_must_be_confirmed() {
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["k"]), &BTreeMap::new());
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::None);
        assert_eq!(s.counters.seen, 1);
    }

    #[test]
    fn a_confirmed_condition_raises() {
        let prior = prior_with("k", KeyState { seen: 1, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::ConfirmRaise);
        assert_eq!(s.counters.seen, 2);
        assert_eq!(s.counters.first, 100);
        assert_eq!(s.counters.flaps, 1);
    }

    #[test]
    fn raise_success_sets_firing_since_and_refreshed() {
        let prior = prior_with("k", KeyState { seen: 1, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        let k = s.on_success(100);
        assert_eq!(k.state, "firing");
        assert_eq!(k.since, 100);
        assert_eq!(k.refreshed, 100);
        assert_eq!(k.flaps, 1);
    }

    #[test]
    fn raise_failure_rolls_back_the_flap_but_not_first() {
        let prior = prior_with("k", KeyState { seen: 1, flaps: 4, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.counters.flaps, 5, "tentatively bumped");
        let k = s.on_failure();
        assert_eq!(k.flaps, 4, "rolled back — bash's own asymmetry");
        assert_eq!(k.first, 100, "NOT rolled back, exactly auron.sh's quirk");
        assert_eq!(k.state, "clear", "never transitioned on a failed write");
    }

    #[test]
    fn a_flap_does_not_accumulate_beads_reraise_still_one_bead() {
        // Already firing, still firing: no new confirm, no flap bump, no trigger.
        let prior = prior_with("k", KeyState { state: "firing".into(), seen: 3, refreshed: 99, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::None);
        assert_eq!(s.counters.flaps, 0);
    }

    #[test]
    fn one_clear_sighting_does_not_retract_it() {
        let prior = prior_with("k", KeyState { state: "firing".into(), refreshed: 100, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&[]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::None, "one absence, CLEAR=2 — not yet");
        assert_eq!(s.counters.unseen, 1);
    }

    #[test]
    fn confirmed_clear_transitions_on_success() {
        let prior = prior_with("k", KeyState { state: "firing".into(), unseen: 1, refreshed: 100, ..Default::default() });
        let steps = step_keys(200, 2, 2, 3600, true, &keys(&[]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::ConfirmClear);
        let k = s.on_success(200);
        assert_eq!(k.state, "clear");
        assert_eq!(k.since, 200);
    }

    #[test]
    fn clear_failure_leaves_state_firing_for_a_retry_next_pass() {
        let prior = prior_with("k", KeyState { state: "firing".into(), unseen: 1, refreshed: 100, ..Default::default() });
        let steps = step_keys(200, 2, 2, 3600, true, &keys(&[]), &prior);
        let s = step_for(&steps, "k");
        let k = s.on_failure();
        assert_eq!(k.state, "firing");
    }

    #[test]
    fn refresh_fires_only_after_the_interval_elapses() {
        let prior = prior_with("k", KeyState { state: "firing".into(), refreshed: 100, ..Default::default() });
        let steps = step_keys(3699, 2, 2, 3600, true, &keys(&["k"]), &prior); // 3599s elapsed
        assert_eq!(step_for(&steps, "k").trigger, Trigger::None);
        let steps = step_keys(3700, 2, 2, 3600, true, &keys(&["k"]), &prior); // 3600s elapsed
        assert_eq!(step_for(&steps, "k").trigger, Trigger::Refresh);
    }

    #[test]
    fn refresh_success_bumps_refreshed_only() {
        let prior = prior_with("k", KeyState { state: "firing".into(), flaps: 2, refreshed: 100, ..Default::default() });
        let steps = step_keys(3700, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        let k = s.on_success(3700);
        assert_eq!(k.refreshed, 3700);
        assert_eq!(k.flaps, 2, "refresh never touches the flap count");
        assert_eq!(k.state, "firing");
    }

    #[test]
    fn refresh_skip_for_a_hand_closed_bead_still_bumps_refreshed() {
        let prior = prior_with("k", KeyState { state: "firing".into(), refreshed: 100, ..Default::default() });
        let steps = step_keys(3700, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        let k = s.on_refresh_skip_closed(3700);
        assert_eq!(k.refreshed, 3700);
        assert_eq!(k.state, "firing", "not reopened — an operator's close is an acknowledgement");
    }

    #[test]
    fn conditions_do_not_mask_each_other() {
        let prior = prior_with("a", KeyState { seen: 1, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, true, &keys(&["a", "b"]), &prior);
        assert_eq!(steps.len(), 2);
        assert_eq!(step_for(&steps, "a").trigger, Trigger::ConfirmRaise);
        assert_eq!(step_for(&steps, "b").trigger, Trigger::None);
        assert_eq!(step_for(&steps, "b").counters.seen, 1);
    }

    #[test]
    fn an_unconfirmed_key_is_dropped_when_it_stops_firing_before_ever_raising() {
        // Seen once, never reaches CONFIRM, then stops — no bead, nothing to clear either.
        let prior = prior_with("k", KeyState { seen: 1, ..Default::default() });
        let steps = step_keys(200, 3, 2, 3600, true, &keys(&[]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::None);
        assert_eq!(s.counters.state, "clear");
    }

    #[test]
    fn an_unreachable_database_updates_counters_but_never_triggers() {
        // Confirmed twice over, but the database never answered this pass: seen/unseen
        // still advance (so the confirm lands the moment it answers again), but no
        // trigger fires and first/flaps are NOT bumped.
        let prior = prior_with("k", KeyState { seen: 1, flaps: 4, ..Default::default() });
        let steps = step_keys(100, 2, 2, 3600, false, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::None);
        assert_eq!(s.counters.seen, 2, "the counter still advances");
        assert_eq!(s.counters.flaps, 4, "not bumped — db_reachable gates the bump itself, not just the write");
        assert_eq!(s.counters.first, 0);
    }

    #[test]
    fn a_confirmed_condition_reopens_after_a_full_clear_and_return() {
        // state=clear after a prior episode, flaps already at 1; firing again reaching
        // CONFIRM raises a second time and the flap count climbs to 2 — one bead, flap-
        // counted, never a second bead for the same cause.
        let prior = prior_with("k", KeyState { state: "clear".into(), seen: 1, flaps: 1, bead: "sp-alert1".into(), ..Default::default() });
        let steps = step_keys(500, 2, 2, 3600, true, &keys(&["k"]), &prior);
        let s = step_for(&steps, "k");
        assert_eq!(s.trigger, Trigger::ConfirmRaise);
        assert_eq!(s.counters.flaps, 2);
        assert_eq!(s.counters.bead, "sp-alert1", "the bead id cache carries through untouched by decide()");
    }
}
