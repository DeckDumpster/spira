//! A section: one component's current and desired document, in the Kubernetes object model
//! field for field (apiVersion, kind, metadata, spec, status) plus the reconciler's own
//! `flow`, `distance` and `actions`. [`admit`] is the schema every write passes; the store's
//! door runs it before the txn, so no writer skips it (law-schema-over-code).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::distance;
use crate::sections::{self, FieldShape, SectionKind};

pub use crate::resource::API_VERSION;

pub const SECTIONS_PREFIX: &str = "telemetry/sections/";
/// Long past any `max_age`, so a stale section stays readable as stale.
pub const SECTION_TTL_SECS: u64 = 7 * 24 * 3600;
/// The actor whose writes are spec: the operator's apply.
pub const COMPOSE: &str = "compose";
/// The longest window a distance term may have in a section that has actions.
pub const MAX_ACTION_WINDOW_SECS: u64 = 3600;
/// The bound on a step's N.
pub const MAX_STEP: u32 = 25;
/// The reconciler's pass record, read in the same snapshot as every section and intent.
pub const PASS_KEY: &str = "telemetry/reconciler/pass";
/// Each pass's record, kept for a day at `<PASSES_PREFIX><n>`.
pub const PASSES_PREFIX: &str = "telemetry/reconciler/passes/";
/// A pass that found the lock held writes here, naming the holder, and does nothing else.
pub const STANDBY_KEY: &str = "telemetry/reconciler/standby";
pub const PASS_HISTORY_TTL_SECS: u64 = 24 * 3600;

pub fn section_key(kind: &str, name: &str) -> String {
    format!("{SECTIONS_PREFIX}{kind}/{name}")
}

/// `<Kind>/<name>` as a CLI names a section.
pub fn parse_ref(r: &str) -> Option<(&str, &str)> {
    let (kind, name) = r.split_once('/')?;
    (valid_segment(kind) && valid_segment(name)).then_some((kind, name))
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.chars().any(char::is_whitespace)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: SectionMeta,
    #[serde(default)]
    pub spec: BTreeMap<String, Target>,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub flow: Flow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<Distance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionMeta {
    pub name: String,
    #[serde(default)]
    pub owner: String,
    /// Bumped by the store on a spec change only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// The store's `mod_rev`, filled in on read; a write carrying one is refused.
    #[serde(
        rename = "resourceVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_version: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(rename = "observedAt", default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_latency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age: Option<String>,
}

/// One desired value and the statute it serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target<T = Value> {
    pub value: T,
    pub statute: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    #[serde(rename = "observedGeneration", default)]
    pub observed_generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The kind's typed fields, checked against its registered Status.
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

/// A numeric status field: its value and the window it is measured over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Numeric {
    pub value: f64,
    pub window: String,
}

/// A windowed field that may hold no number: its value, `"WarmingUp"` while its window still
/// reaches back before a release of the world from a halt at least that long, or what made
/// it unobservable. Only a value is ever a term.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Windowed {
    Value(Numeric),
    Unobservable(Unobservable),
    WarmingUp(WarmingUp),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unobservable {
    pub unobservable: String,
    pub window: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarmingUp {
    WarmingUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: ConditionStatus,
    #[serde(default)]
    pub reason: String,
    /// What one condition of a type is about, when a type can stand for several things at once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The runbook entry that answers the condition.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runbook: String,
    #[serde(rename = "lastTransitionTime")]
    pub last_transition_time: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

/// The terms this section reads and writes, as `<section>/<term>` names.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distance {
    #[serde(rename = "fn")]
    pub expr: String,
    pub open: f64,
    pub close: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub name: String,
    pub class: ActionClass,
    pub step: String,
    pub moves: String,
    pub addresses: Vec<String>,
    pub expected_effect: f64,
    pub horizon: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopping: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionClass {
    Local,
    CapacityRaise,
    CapacityLower,
    Destructive,
}

/// A refused write: the field it names, the rule it broke, and a short code for metrics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub field: String,
    pub rule: String,
    pub code: &'static str,
}

impl Refusal {
    pub fn new(field: impl Into<String>, code: &'static str, rule: impl Into<String>) -> Self {
        Refusal {
            field: field.into(),
            rule: rule.into(),
            code,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.rule)
    }
}

/// `level`, or a duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Level,
    Secs(u64),
}

impl Window {
    pub fn parse(s: &str) -> Option<Window> {
        if s == "level" {
            return Some(Window::Level);
        }
        parse_duration(s).map(Window::Secs)
    }

    pub fn secs(self) -> u64 {
        match self {
            Window::Level => 0,
            Window::Secs(s) => s,
        }
    }
}

/// `<N><s|m|h|d>`, N a positive integer.
pub fn parse_duration(s: &str) -> Option<u64> {
    let unit = s.chars().last()?;
    let mult = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return None,
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    (n > 0).then(|| n.checked_mul(mult)).flatten()
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the one form this harness stamps, as epoch seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    if b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = &s[r];
        t.bytes().all(|c| c.is_ascii_digit()).then(|| t.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    // days_from_civil (Howard Hinnant), the inverse of the store's formatter.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec).ok()
}

/// Parses a section from the wire, naming the first unknown or malformed field by its path.
pub fn parse_section(v: &Value) -> Result<Section, Refusal> {
    serde_path_to_error::deserialize(v).map_err(|e| {
        let path = e.path().to_string();
        let field = if path.is_empty() || path == "." {
            "section".to_string()
        } else {
            path
        };
        Refusal::new(field, "schema", e.inner().to_string())
    })
}

/// The kind `kind` names, or the refusal: an unregistered kind is refused, Andon by name.
pub fn registered(kind: &str) -> Result<&'static SectionKind, Refusal> {
    if kind == sections::ANDON {
        return Err(Refusal::new(
            "kind",
            "unknown-kind",
            "Andon is not registered: the andon cord's own design adds it, and until then it is absent",
        ));
    }
    sections::lookup(kind)
        .ok_or_else(|| Refusal::new("kind", "unknown-kind", format!("{kind:?} is not a registered section kind")))
}

impl Section {
    /// A numeric status field the kind declares, with its window, or `None`.
    pub fn numeric(&self, kind: &SectionKind, name: &str) -> Option<(f64, Window)> {
        if !matches!(kind.shape(name), Some(FieldShape::Numeric | FieldShape::Windowed)) {
            return None;
        }
        let n: Numeric = serde_json::from_value(self.status.fields.get(name)?.clone()).ok()?;
        Some((n.value, Window::parse(&n.window)?))
    }

    /// The distance's value over this section's status.
    pub fn distance_value(&self, kind: &SectionKind) -> Option<Result<f64, String>> {
        let d = self.distance.as_ref()?;
        Some(distance::parse(&d.expr).and_then(|e| e.eval(&|f| self.numeric(kind, f).map(|n| n.0))))
    }
}

fn duration_field(field: &str, v: &Option<String>) -> Result<Option<u64>, Refusal> {
    match v {
        None => Ok(None),
        Some(s) => parse_duration(s)
            .map(Some)
            .ok_or_else(|| Refusal::new(field, "duration", format!("{s:?} is not a duration (<N>s|m|h|d)"))),
    }
}

/// A step in one of the closed forms: `+N`, `−N` (each optionally naming its unit),
/// `N% fewer|more` with 1 ≤ N ≤ 25, or `one <thing>`.
pub fn parse_step(step: &str) -> Result<(), String> {
    let words: Vec<&str> = step.split_whitespace().collect();
    let bounded = |n: &str| -> Result<(), String> {
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{n:?} is not a whole number"));
        }
        match n.parse::<u32>() {
            Ok(v) if (1..=MAX_STEP).contains(&v) => Ok(()),
            _ => Err(format!("N must be 1..={MAX_STEP}, not {n}")),
        }
    };
    let unit = |rest: &[&str]| {
        rest.iter()
            .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
    };
    let refuse = || {
        Err(format!(
            "{step:?} is not a bounded step (+N, −N, N% fewer|more with 1 ≤ N ≤ {MAX_STEP}, or one <thing>)"
        ))
    };
    if words.contains(&"rebuild") {
        return refuse();
    }
    match words.as_slice() {
        ["one", thing @ ..] if !thing.is_empty() && unit(thing) => Ok(()),
        [pct, dir] if (*dir == "fewer" || *dir == "more") && pct.ends_with('%') => {
            bounded(&pct[..pct.len() - 1])
        }
        [n, rest @ ..] if unit(rest) => {
            let digits = n
                .strip_prefix('+')
                .or_else(|| n.strip_prefix('-'))
                .or_else(|| n.strip_prefix('−'));
            match digits {
                Some(d) => bounded(d),
                None => refuse(),
            }
        }
        _ => refuse(),
    }
}

fn flow_name(field: &str, name: &str) -> Result<(), Refusal> {
    let ok = name
        .rsplit_once('/')
        .is_some_and(|(s, t)| !s.is_empty() && !t.is_empty() && !name.chars().any(char::is_whitespace));
    if ok {
        Ok(())
    } else {
        Err(Refusal::new(field, "flow", format!("{name:?} is not a <section>/<term> name")))
    }
}

/// The schema: everything a write must satisfy whoever writes it. Ownership, revisions and
/// generation are the store's to judge, beside this.
pub fn admit(s: &Section) -> Result<&'static SectionKind, Refusal> {
    if s.api_version != API_VERSION {
        return Err(Refusal::new(
            "apiVersion",
            "api-version",
            format!("{:?} is not {API_VERSION:?}", s.api_version),
        ));
    }
    let kind = registered(&s.kind)?;
    if !valid_segment(&s.metadata.name) {
        return Err(Refusal::new("metadata.name", "name", format!("{:?} is not a name", s.metadata.name)));
    }
    if s.metadata.owner != kind.owner {
        return Err(Refusal::new(
            "metadata.owner",
            "owner",
            format!("{} sections are owned by {:?}, not {:?}", kind.name, kind.owner, s.metadata.owner),
        ));
    }
    if s.metadata.resource_version.is_some() {
        return Err(Refusal::new(
            "metadata.resourceVersion",
            "resource-version",
            "is the store's revision, filled in on read and never written",
        ));
    }
    if let Some(at) = &s.metadata.observed_at {
        if parse_rfc3339(at).is_none() {
            return Err(Refusal::new("metadata.observedAt", "time", format!("{at:?} is not YYYY-MM-DDTHH:MM:SSZ")));
        }
    }
    let target_latency = duration_field("metadata.target_latency", &s.metadata.target_latency)?;
    duration_field("metadata.max_age", &s.metadata.max_age)?;

    let spec = serde_json::to_value(&s.spec).unwrap_or(Value::Null);
    (kind.check_spec)(&spec).map_err(|e| Refusal::new(format!("spec.{}", e.0), "schema", e.1))?;
    let fields = Value::Object(s.status.fields.clone().into_iter().collect());
    (kind.check_status)(&fields).map_err(|e| Refusal::new(format!("status.{}", e.0), "schema", e.1))?;
    for (name, shape) in kind.fields {
        let Some(v) = s.status.fields.get(*name) else { continue };
        let numeric = s.numeric(kind, name).is_some();
        let ok = match shape {
            FieldShape::Numeric => numeric,
            FieldShape::Windowed => match serde_json::from_value::<Windowed>(v.clone()) {
                Ok(Windowed::Value(_)) => numeric,
                Ok(Windowed::Unobservable(u)) => !u.unobservable.trim().is_empty() && Window::parse(&u.window).is_some(),
                Ok(Windowed::WarmingUp(_)) => true,
                Err(_) => false,
            },
            FieldShape::List => true,
        };
        if !ok {
            let rule = match shape {
                FieldShape::Windowed => "is not a { value, window }, a { unobservable, window } with a reason, or \"WarmingUp\"",
                _ => "is neither level nor a duration",
            };
            return Err(Refusal::new(format!("status.{name}.window"), "window", rule));
        }
    }

    for (i, c) in s.status.conditions.iter().enumerate() {
        let field = format!("status.conditions[{i}]");
        if kind.condition(&c.type_).is_none() {
            return Err(Refusal::new(
                format!("{field}.type"),
                "condition",
                format!("{:?} is not a condition type {} declares", c.type_, kind.name),
            ));
        }
        if c.status == ConditionStatus::Unknown && c.reason.trim().is_empty() {
            return Err(Refusal::new(format!("{field}.reason"), "condition", "an Unknown condition must say why"));
        }
        if parse_rfc3339(&c.last_transition_time).is_none() {
            return Err(Refusal::new(format!("{field}.lastTransitionTime"), "time", "is not YYYY-MM-DDTHH:MM:SSZ"));
        }
    }

    for (i, n) in s.flow.consumes.iter().enumerate() {
        flow_name(&format!("flow.consumes[{i}]"), n)?;
    }
    for (i, n) in s.flow.produces.iter().enumerate() {
        flow_name(&format!("flow.produces[{i}]"), n)?;
    }

    if let Some(d) = &s.distance {
        if d.close >= d.open {
            return Err(Refusal::new("distance.close", "hysteresis", format!("close {} must be below open {}", d.close, d.open)));
        }
        let expr = distance::parse(&d.expr).map_err(|e| Refusal::new("distance.fn", "grammar", e))?;
        for term in expr.fields() {
            let Some((_, window)) = s.numeric(kind, term) else {
                return Err(Refusal::new(
                    "distance.fn",
                    "term",
                    format!("{term} is not a numeric status field of this section"),
                ));
            };
            if !s.actions.is_empty() && window.secs() > MAX_ACTION_WINDOW_SECS {
                return Err(Refusal::new(
                    "distance.fn",
                    "window",
                    format!(
                        "{term} is measured over {}s; a section with actions measures distance over levels and windows of at most 1h",
                        window.secs()
                    ),
                ));
            }
        }
    }

    let mut declared = std::collections::BTreeSet::new();
    for (i, a) in s.actions.iter().enumerate() {
        let field = format!("actions[{i}]");
        if !declared.insert((a.name.as_str(), a.class)) {
            return Err(Refusal::new(format!("{field}.name"), "action", format!("{} is declared twice for one class", a.name)));
        }
        if a.stopping == Some(true) {
            return Err(Refusal::new(format!("{field}.stopping"), "stopping", "stops belong to the andon cord, never to a section"));
        }
        parse_step(&a.step).map_err(|e| Refusal::new(format!("{field}.step"), "step", e))?;
        let Some((_, window)) = s.numeric(kind, &a.moves) else {
            return Err(Refusal::new(
                format!("{field}.moves"),
                "moves",
                format!("{} is not a numeric status field of this section", a.moves),
            ));
        };
        for (j, c) in a.addresses.iter().enumerate() {
            if kind.condition(c).is_none() {
                return Err(Refusal::new(
                    format!("{field}.addresses[{j}]"),
                    "addresses",
                    format!("{c:?} is not a condition type {} declares", kind.name),
                ));
            }
        }
        let horizon = parse_duration(&a.horizon)
            .ok_or_else(|| Refusal::new(format!("{field}.horizon"), "duration", format!("{:?} is not a duration", a.horizon)))?;
        let Some(latency) = target_latency else {
            return Err(Refusal::new("metadata.target_latency", "horizon", "a section with actions declares its target_latency"));
        };
        let floor = window.secs() + latency;
        if horizon < floor {
            return Err(Refusal::new(
                format!("{field}.horizon"),
                "horizon",
                format!(
                    "{horizon}s is shorter than {}'s window ({}s) plus target_latency ({latency}s)",
                    a.moves,
                    window.secs()
                ),
            ));
        }
    }
    Ok(kind)
}

/// An owner's publish declares when it observed and how long that stays good.
pub fn admit_publish(s: &Section) -> Result<&'static SectionKind, Refusal> {
    let kind = admit(s)?;
    for (field, v) in [
        ("metadata.observedAt", &s.metadata.observed_at),
        ("metadata.target_latency", &s.metadata.target_latency),
        ("metadata.max_age", &s.metadata.max_age),
    ] {
        if v.is_none() {
            return Err(Refusal::new(field, "required", "a status publish declares it"));
        }
    }
    Ok(kind)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum Freshness {
    Fresh,
    Stale {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        age_secs: Option<u64>,
        reason: String,
    },
}

/// The one freshness rule: fresh while `now − observedAt ≤ max_age`.
pub fn freshness(s: &Section, now_ms: u64) -> Freshness {
    let now = now_ms / 1000;
    let Some(observed) = s.metadata.observed_at.as_deref().and_then(parse_rfc3339) else {
        return Freshness::Stale {
            age_secs: None,
            reason: "no status has been published".into(),
        };
    };
    let age = now.saturating_sub(observed);
    let Some(max_age) = s.metadata.max_age.as_deref().and_then(parse_duration) else {
        return Freshness::Stale {
            age_secs: Some(age),
            reason: "no max_age is declared".into(),
        };
    };
    if age <= max_age {
        Freshness::Fresh
    } else {
        Freshness::Stale {
            age_secs: Some(age),
            reason: format!("observed {age}s ago, past max_age {max_age}s"),
        }
    }
}

/// Generation lag is its own fact: a fresh section may still be behind its spec.
pub fn caught_up(s: &Section) -> bool {
    s.status.observed_generation >= s.metadata.generation.unwrap_or(0)
}

#[cfg(test)]
mod tests;
