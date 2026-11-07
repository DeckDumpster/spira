//! The goodput pass's record: written once per pass by the pass, read back by the next pass for
//! hysteresis and numbering, and by the alarms component and the pane. The latest is at
//! [`PASS_KEY`]; each pass is also kept at [`history_key`] for [`PASS_HISTORY_TTL_SECS`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spira_desired_state::intent::JudgementResult;
use spira_desired_state::section::Freshness;

pub use spira_desired_state::section::{PASSES_PREFIX, PASS_HISTORY_TTL_SECS, PASS_KEY, STANDBY_KEY};

pub fn history_key(n: u64) -> String {
    format!("{PASSES_PREFIX}{n}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Acting,
    Standby,
    Halted,
    Shadow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PassRecord {
    pub n: u64,
    pub list_rev: u64,
    pub at: String,
    pub holder: String,
    pub mode: Mode,
    #[serde(default)]
    pub goodput: Option<Goodput>,
    /// The top-ranked gap, as `<Kind>/<name>`.
    #[serde(default)]
    pub binding: Option<String>,
    /// The gaps open at the end of this pass: the next pass's hysteresis reads these.
    #[serde(default)]
    pub gaps: Vec<Gap>,
    /// Intents written and read back.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Steps planned and not written: the kind is shadowed or the world is halted.
    #[serde(default)]
    pub would_step: Vec<Step>,
    #[serde(default)]
    pub judged: Vec<Judged>,
    /// Owners' refusals acknowledged since the previous pass: re-planned, never a failure.
    #[serde(default)]
    pub acks_refused: Vec<AckRefused>,
    #[serde(default)]
    pub refused: Vec<Refused>,
    #[serde(default)]
    pub exhausted: Vec<String>,
    #[serde(default)]
    pub no_lever: Vec<NoLever>,
    #[serde(default)]
    pub ages: BTreeMap<String, SectionAge>,
    #[serde(default)]
    pub shadow_kinds: Vec<String>,
}

/// `None` while the window is still warming up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Goodput {
    pub ratio: Option<f64>,
    pub rate_per_day: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub section: String,
    pub distance: f64,
    pub score: f64,
    /// The `produces` chain from this section to `lifecycle/landed`; empty when none.
    #[serde(default)]
    pub path_to_goodput: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub section: String,
    pub action: String,
    /// The knob the owner moves from `from` to `to`.
    pub target: String,
    pub from: serde_json::Value,
    pub to: serde_json::Value,
    /// The intent key the step was written at; `None` for a would-step, which writes none.
    pub intent: Option<String>,
    /// Why a would-step was not written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Judged {
    pub intent: String,
    pub result: JudgementResult,
    pub baseline: f64,
    pub observed: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AckRefused {
    pub intent: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub section: String,
    pub action: String,
    /// A short, stable code for metrics: `stale-input`, `upstream-absent`, …
    pub code: String,
    pub reason: String,
}

/// An open gap's open conditions that no declared action addresses, recorded beside any step
/// the gap takes. `since` is the pass that first recorded it, carried while the same conditions
/// stand: a standing NoLever is recorded once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoLever {
    pub section: String,
    pub conditions: Vec<String>,
    pub since: u64,
}

/// What one pass saw of one section's publishing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionAge {
    /// Seconds since `observedAt`; `None` when no status has been published.
    pub age_secs: Option<u64>,
    pub freshness: Freshness,
    /// `observedGeneration` has reached `generation`; false reads as NotCaughtUp.
    pub caught_up: bool,
    pub publish_latency: PublishLatency,
}

/// The age against the owner's `target_latency` promise: a miss is a measured term, never a
/// second freshness state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PublishLatency {
    Within { target_secs: u64 },
    Late { target_secs: u64, over_secs: u64 },
    /// No `target_latency` is declared.
    Undeclared,
    /// No status has been published, so there is no age to measure.
    Unmeasured,
}

impl PassRecord {
    /// The number the pass after this record takes.
    pub fn next_n(prev: Option<&PassRecord>) -> u64 {
        prev.map_or(1, |p| p.n + 1)
    }
}
