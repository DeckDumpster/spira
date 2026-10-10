//! The hold sweep: every held bead is visited each pass and moved one step for its kind.
//! [`decide`] is the pure rule; [`sweep`] walks the holds through an [`Env`] seam so a fixture
//! stands in for the lifecycle store and the mailboxes.
//!
//! The sweep never lifts an `ask` or a `poison`: it routes them to someone who can decide.
//! A missing timestamp is never old — a hold with no recorded `since` gets no time-based
//! action, because treating it as ancient would act on beads it cannot account for.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const ASK_RESURFACE_SECS: u64 = 24 * 3600;
pub const POISON_DIAGNOSE_SECS: u64 = 24 * 3600;
pub const MANUAL_REMIND_SECS: u64 = 72 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Wait,
    Ask,
    Poison,
    Operator,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Wait, Kind::Ask, Kind::Poison, Kind::Operator];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Wait => "wait",
            Kind::Ask => "ask",
            Kind::Poison => "poison",
            Kind::Operator => "operator",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub bead: String,
    pub kind: Kind,
    /// When this hold was placed; `None` when the store carries no record of it.
    pub since: Option<u64>,
    /// The hold's own reason text, verbatim.
    pub detail: Option<String>,
    /// Who placed it, when the store records that.
    pub actor: Option<String>,
}

impl Hold {
    pub fn key(&self) -> String {
        key_of(&self.bead, self.kind)
    }
}

pub fn key_of(bead: &str, kind: Kind) -> String {
    format!("holds:{bead}:{}", kind.as_str())
}

/// The newest delivered question about a bead, and the mailbox it went to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskMail {
    pub delivered_at: u64,
    pub mailbox: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    LiftWait { until: u64 },
    RaiseAsk,
    ResurfaceAsk { delivered_at: u64, mailbox: String },
    DiagnosePoison,
    RemindManual,
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::LiftWait { .. } => "lift-wait",
            Action::RaiseAsk => "raise-ask",
            Action::ResurfaceAsk { .. } => "resurface-ask",
            Action::DiagnosePoison => "diagnose-poison",
            Action::RemindManual => "remind-manual",
        }
    }
}

/// `ask` is the newest delivered question for this bead; `last_acted` is when the sweep last
/// acted on this hold. Each kind acts at its threshold and not before.
pub fn decide(now: u64, hold: &Hold, ask: Option<&AskMail>, last_acted: Option<u64>) -> Option<Action> {
    let age = hold.since.map(|s| now.saturating_sub(s));
    match hold.kind {
        Kind::Wait => {
            let until = spira_config::lc_state::snooze_until(hold.detail.as_deref()?)?;
            let until = u64::try_from(until).ok()?;
            (now > until).then_some(Action::LiftWait { until })
        }
        Kind::Ask => match ask {
            None => {
                let recently = last_acted.is_some_and(|t| now.saturating_sub(t) < ASK_RESURFACE_SECS);
                (!recently).then_some(Action::RaiseAsk)
            }
            Some(m) => {
                let stale = now.saturating_sub(m.delivered_at) >= ASK_RESURFACE_SECS;
                let done = last_acted.is_some_and(|t| t >= m.delivered_at);
                (stale && !done).then(|| Action::ResurfaceAsk { delivered_at: m.delivered_at, mailbox: m.mailbox.clone() })
            }
        },
        Kind::Poison => {
            let since = hold.since?;
            let done = last_acted.is_some_and(|t| t >= since);
            (age? >= POISON_DIAGNOSE_SECS && !done).then_some(Action::DiagnosePoison)
        }
        Kind::Operator => {
            let due = match last_acted {
                Some(t) => now.saturating_sub(t) >= MANUAL_REMIND_SECS,
                None => true,
            };
            (age? >= MANUAL_REMIND_SECS && due).then_some(Action::RemindManual)
        }
    }
}

/// Persisted between passes: when the sweep last ran and when it last acted on each hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepState {
    pub last_pass: u64,
    pub acted: BTreeMap<String, u64>,
}

pub fn load_state(path: &std::path::Path) -> SweepState {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_state(path: &std::path::Path, state: &SweepState) -> std::io::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string()))
}

/// What the sweep reads and does. Reads fail closed: an `Err` means "could not tell", and the
/// sweep skips what depends on it rather than reading it as "nothing there".
pub trait Env {
    fn holds(&mut self) -> Result<Vec<Hold>, String>;
    /// The newest delivered question per bead.
    fn asks(&mut self) -> Result<BTreeMap<String, AskMail>, String>;
    fn act(&mut self, hold: &Hold, action: &Action) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taken {
    pub key: String,
    pub bead: String,
    pub kind: Kind,
    pub action: Action,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The kill switch held the action back.
    Shadowed,
    Failed(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub visited: usize,
    pub taken: Vec<Taken>,
    /// Keys whose hold needed a step when this pass read it, whether or not the step was taken:
    /// a key acted on last pass and wanted again now is a step that did not take.
    pub wanted: BTreeSet<String>,
    /// Keys of every hold seen this pass.
    pub seen: BTreeSet<String>,
    pub skipped: Vec<String>,
}

/// Visits every hold once. `shadow` records what would be done and does none of it.
pub fn sweep(now: u64, env: &mut dyn Env, state: &mut SweepState, shadow: bool) -> Result<Report, String> {
    let holds = env.holds()?;
    let asks = env.asks();
    let mut report = Report { visited: holds.len(), ..Report::default() };
    for hold in &holds {
        let key = hold.key();
        report.seen.insert(key.clone());
        let ask = match (hold.kind, &asks) {
            (Kind::Ask, Err(e)) => {
                report.skipped.push(format!("{key}: mailboxes unreadable: {e}"));
                continue;
            }
            (Kind::Ask, Ok(m)) => m.get(&hold.bead),
            _ => None,
        };
        let Some(action) = decide(now, hold, ask, state.acted.get(&key).copied()) else { continue };
        report.wanted.insert(key.clone());
        let outcome = if shadow {
            Outcome::Shadowed
        } else {
            match env.act(hold, &action) {
                Ok(()) => {
                    state.acted.insert(key.clone(), now);
                    Outcome::Done
                }
                Err(e) => Outcome::Failed(e),
            }
        };
        report.taken.push(Taken { key, bead: hold.bead.clone(), kind: hold.kind, action, outcome });
    }
    state.acted.retain(|k, _| report.seen.contains(k));
    state.last_pass = now;
    Ok(report)
}

#[cfg(test)]
mod tests;
