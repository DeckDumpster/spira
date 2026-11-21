//! What a caller did after a read was ended, ran past its limit, or waited at the read gate:
//! asked the same verb again and got an answer (`answered`), or did not (`surfaced`).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const READ_FOLLOWUPS: &str = "spira_lc_read_followups";

/// How long a caller has to ask again before its failure counts as surfaced.
pub const FOLLOWUP_WINDOW: Duration = Duration::from_secs(30);

/// Keys held pending at once; the oldest settles `surfaced` to make room.
const MAX_PENDING: usize = 256;

type Pair = (String, String);

#[derive(Default)]
pub struct Followups {
    pending: BTreeMap<Pair, Duration>,
    pub answered: BTreeMap<String, u64>,
    pub surfaced: BTreeMap<String, u64>,
}

impl Followups {
    fn settle(&mut self, caller: &str, outcome_answered: bool) {
        let m = if outcome_answered { &mut self.answered } else { &mut self.surfaced };
        *m.entry(caller.to_string()).or_default() += 1;
    }

    pub fn expire(&mut self, now: Duration) {
        let late: Vec<Pair> = self.pending.iter().filter(|(_, at)| now.saturating_sub(**at) > FOLLOWUP_WINDOW).map(|(k, _)| k.clone()).collect();
        for k in late {
            self.pending.remove(&k);
            self.settle(&k.0, false);
        }
    }

    /// One finished request. `touched`: a read ended, overran, or waited at the gate.
    pub fn observe(&mut self, now: Duration, caller: &str, verb: &str, touched: bool, exit: i32) {
        self.expire(now);
        let key = (caller.to_string(), verb.to_string());
        if exit == 0 {
            if self.pending.remove(&key).is_some() {
                self.settle(caller, true);
            }
            if touched {
                self.settle(caller, true);
            }
        } else if touched && !self.pending.contains_key(&key) {
            if self.pending.len() >= MAX_PENDING {
                if let Some(oldest) = self.pending.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone()) {
                    self.pending.remove(&oldest);
                    self.settle(&oldest.0, false);
                }
            }
            self.pending.insert(key, now);
        }
    }

    pub fn series_since(&self, pushed: &(BTreeMap<String, u64>, BTreeMap<String, u64>), at: std::time::SystemTime) -> Vec<spira_otel::metrics::Series> {
        let mut out = Vec::new();
        for (outcome, now, before) in [("answered", &self.answered, &pushed.0), ("surfaced", &self.surfaced, &pushed.1)] {
            for (caller, n) in now {
                let delta = n - before.get(caller).copied().unwrap_or(0);
                if delta > 0 {
                    out.push(spira_otel::metrics::Series {
                        name: READ_FOLLOWUPS.into(),
                        value: delta as f64,
                        labels: vec![("caller".to_string(), caller.clone()), ("outcome".to_string(), outcome.to_string())],
                        at,
                    });
                }
            }
        }
        out
    }
}

static FOLLOWUPS: Mutex<Followups> = Mutex::new(Followups { pending: BTreeMap::new(), answered: BTreeMap::new(), surfaced: BTreeMap::new() });
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now() -> Duration {
    START.get_or_init(Instant::now).elapsed()
}

pub fn request_done(caller: &str, verb: &str, touched: bool, exit: i32) {
    if let Ok(mut f) = FOLLOWUPS.lock() {
        f.observe(now(), caller, verb, touched, exit);
    }
}

pub type Pushed = (BTreeMap<String, u64>, BTreeMap<String, u64>);

/// Pushes the settled counts that moved since `pushed`; it advances only when the push succeeds.
pub fn push(sink: &spira_otel::metrics::MetricsSink, pushed: &mut Pushed) {
    let Ok(mut f) = FOLLOWUPS.lock() else { return };
    f.expire(now());
    let series = f.series_since(pushed, std::time::SystemTime::now());
    let current = (f.answered.clone(), f.surfaced.clone());
    drop(f);
    if !series.is_empty() && sink.try_push_series(&series).is_ok() {
        *pushed = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn total(f: &Followups) -> (u64, u64) {
        (f.answered.values().sum(), f.surfaced.values().sum())
    }

    #[test]
    fn asking_again_inside_the_window_and_getting_an_answer_is_answered() {
        let mut f = Followups::default();
        f.observe(S(0), "spira-runtime", "list", true, 2);
        f.observe(S(5), "spira-runtime", "list", false, 0);
        f.expire(S(100));
        assert_eq!((f.answered["spira-runtime"], total(&f).1), (1, 0));
    }

    #[test]
    fn no_follow_up_past_the_window_is_surfaced() {
        let mut f = Followups::default();
        f.observe(S(0), "spira-runtime", "list", true, 2);
        f.expire(S(30));
        assert_eq!(total(&f), (0, 0), "still inside the window");
        f.expire(S(31));
        assert_eq!((f.surfaced["spira-runtime"], total(&f).0), (1, 0));
    }

    #[test]
    fn a_follow_up_from_another_caller_or_verb_does_not_settle_it() {
        let mut f = Followups::default();
        f.observe(S(0), "spira-runtime", "list", true, 2);
        f.observe(S(1), "lc-view", "list", false, 0);
        f.observe(S(2), "spira-runtime", "show", false, 0);
        assert_eq!(total(&f), (0, 0));
        f.expire(S(60));
        assert_eq!(f.surfaced["spira-runtime"], 1);
    }

    #[test]
    fn a_touched_request_that_answered_is_answered_at_once() {
        let mut f = Followups::default();
        f.observe(S(0), "work", "plan", true, 0);
        assert_eq!(f.answered["work"], 1);
    }

    #[test]
    fn an_untouched_request_counts_nothing() {
        let mut f = Followups::default();
        f.observe(S(0), "work", "plan", false, 0);
        f.observe(S(1), "work", "plan", false, 2);
        f.expire(S(99));
        assert_eq!(total(&f), (0, 0));
    }

    #[test]
    fn a_late_follow_up_settles_the_expired_entry_as_surfaced_first() {
        let mut f = Followups::default();
        f.observe(S(0), "hel", "list", true, 2);
        f.observe(S(40), "hel", "list", false, 0);
        assert_eq!(total(&f), (0, 1));
    }

    #[test]
    fn pending_is_bounded_and_the_oldest_surfaces() {
        let mut f = Followups::default();
        for i in 0..MAX_PENDING + 3 {
            f.observe(Duration::from_millis(i as u64), &format!("c{i}"), "list", true, 2);
        }
        assert_eq!(f.pending.len(), MAX_PENDING);
        assert_eq!(total(&f).1, 3);
    }

    #[test]
    fn series_carry_caller_and_outcome_labels_and_only_the_delta() {
        let mut f = Followups::default();
        f.observe(S(0), "bead", "list", true, 0);
        f.observe(S(0), "hel", "list", true, 2);
        f.expire(S(99));
        let got = f.series_since(&Default::default(), std::time::SystemTime::now());
        let has = |caller: &str, outcome: &str| got.iter().any(|s| s.name == READ_FOLLOWUPS && s.labels == [("caller".to_string(), caller.to_string()), ("outcome".to_string(), outcome.to_string())] && s.value == 1.0);
        assert!(has("bead", "answered") && has("hel", "surfaced"));
        let pushed = (f.answered.clone(), f.surfaced.clone());
        assert!(f.series_since(&pushed, std::time::SystemTime::now()).is_empty());
    }
}
