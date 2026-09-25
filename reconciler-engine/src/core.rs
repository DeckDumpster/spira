//! The pure half: one invariant's previous hysteresis state plus one pass's raw reading
//! gives the next state and this pass's verdict. Nothing here reads a clock, a file or a
//! socket, so every grace-in / clean-pass-out / remedy-verification transition is a fixture
//! a test can replay (sp-pu7v6).
//!
//! UNOBSERVABLE IS NEVER SATISFIED (law-a-control-that-cannot-check-must-refuse): the only
//! way `Verdict.status` is `Satisfied` is for `raw` to already be `Satisfied` — an
//! `Unobservable` reading is carried straight through, never smoothed into "fine".

use serde::{Deserialize, Serialize};

/// What one pass observed for one invariant, before hysteresis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawStatus {
    Satisfied,
    /// `since_hint` is the true since-when when the observation itself carries one (e.g. a
    /// forge `queued-since` timestamp) — more accurate than the pass on which czar-pass
    /// first happened to notice. `None` falls back to that first-noticed pass.
    Gap { desired: String, observed: String, since_hint: Option<u64> },
    Unobservable { reason: String },
}

impl RawStatus {
    fn is_satisfied(&self) -> bool {
        matches!(self, RawStatus::Satisfied)
    }
}

/// Per-invariant memory carried from pass to pass. Persisted verbatim by the IO seam so a
/// restart of czar-pass does not forget a streak already in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HysteresisState {
    /// When the current not-satisfied streak began. `None` while satisfied.
    since: Option<u64>,
    /// When a deterministic remedy was last attempted for this streak. Cleared once the
    /// streak closes, or once it is found still open (that attempt is now judged to have
    /// failed, so the next one is a fresh attempt, not a second silent retry mistaken for
    /// the first).
    remedy_attempted_at: Option<u64>,
    remedy_desc: Option<String>,
}

/// One invariant's outcome for this pass: what the time series records (`status`, always
/// the raw reading — never smoothed) and what the remedy ladder does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub status: RawStatus,
    /// When the current not-satisfied streak began. Set even inside the grace period, so
    /// an escalation can say how long the thing has actually been true.
    pub since: Option<u64>,
    /// True once the streak has outlasted its grace period. A gap inside grace raises
    /// nothing: gate any remedy or escalation on this, not on `status` alone.
    pub is_gap: bool,
    /// True on the one pass a streak resolves. One clean `Satisfied` pass is enough to
    /// close — this is deliberately not "two in a row".
    pub just_closed: bool,
    /// A remedy was attempted on a prior pass for this same streak, and the streak is
    /// still a gap: the remedy did not close it, so it does not count as a remedy and the
    /// caller must escalate rather than reattempt it silently.
    pub remedy_failed: bool,
}

/// Advances one invariant by one pass. Pure: `(now, raw, grace_secs, prev)` always yields
/// the same `(Verdict, HysteresisState)`.
pub fn step(now: u64, raw: RawStatus, grace_secs: u64, prev: HysteresisState) -> (Verdict, HysteresisState) {
    if raw.is_satisfied() {
        let just_closed = prev.since.is_some();
        let verdict = Verdict { status: raw, since: None, is_gap: false, just_closed, remedy_failed: false };
        return (verdict, HysteresisState::default());
    }

    let since_hint = match &raw {
        RawStatus::Gap { since_hint, .. } => *since_hint,
        _ => None,
    };
    let since = since_hint.or(prev.since).unwrap_or(now);
    let age = now.saturating_sub(since);
    let is_gap = age >= grace_secs;
    let remedy_failed = is_gap && prev.remedy_attempted_at.is_some();

    let next = HysteresisState {
        since: Some(since),
        remedy_attempted_at: if remedy_failed { None } else { prev.remedy_attempted_at },
        remedy_desc: if remedy_failed { None } else { prev.remedy_desc },
    };
    let verdict = Verdict { status: raw, since: Some(since), is_gap, just_closed: false, remedy_failed };
    (verdict, next)
}

/// Records that a deterministic remedy was attempted this pass, for verification on the
/// next: [`step`] sets [`Verdict::remedy_failed`] if the invariant is still a gap then.
pub fn record_remedy(state: &mut HysteresisState, now: u64, desc: &str) {
    state.remedy_attempted_at = Some(now);
    state.remedy_desc = Some(desc.to_string());
}

/// The description of the last remedy attempted for this streak, if any — for an
/// escalation to say what was already tried.
pub fn last_remedy(state: &HysteresisState) -> Option<&str> {
    state.remedy_desc.as_deref()
}

#[cfg(test)]
mod tests;
