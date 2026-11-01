//! The action selector: the ranked gaps and every intent in, at most one step per gap out.
//! Pure: it reads only the assembled documents, so a fresh process plans what an uninterrupted
//! one would. Every rule fails closed: a step whose inputs cannot be shown fresh, whose upstream
//! cannot be seen, or whose knob cannot be read is refused with its reason, never taken.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use spira_desired_state::intent::{parse_key, AckResult, By, Intent, JudgementResult};
use spira_desired_state::section::{parse_duration, parse_rfc3339, Action, ActionClass, ConditionStatus, Freshness};
use spira_desired_state::sections::ANDON;
use spira_desired_state::store::format_rfc3339;

use crate::assemble::{Assembled, Current, Derived};
use crate::pass_record::{AckRefused, NoLever, Refused};

/// The condition the assembler derives for intents an owner has not acknowledged in time.
pub const INTENTS_UNACKNOWLEDGED: &str = "IntentsUnacknowledged";
/// Back-off never exceeds a day; intents outlive it, so the history it reads is always there.
pub const BACKOFF_CAP_SECS: u64 = 24 * 3600;

/// One ranked section: the ranking's output, walked from the top. Each open gap gets at most
/// one step; NoLever annotates the gap's unaddressed conditions beside whatever it gets.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedGap {
    pub section: String,
    pub distance: f64,
    pub score: f64,
    pub open: bool,
}

pub struct Inputs<'a> {
    pub assembled: &'a Assembled,
    pub gaps: &'a [RankedGap],
    pub shadow: &'a [String],
    pub halted: bool,
    pub now_ms: u64,
    /// The lock token every intent this pass writes carries.
    pub token: &'a str,
}

/// A step the selector chose, with the intent that takes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub section: String,
    pub action: String,
    pub key: String,
    pub intent: Intent,
    /// Why it is not written: the kind is shadowed or the world is halted.
    pub held: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Judge {
    pub key: String,
    pub section: String,
    pub result: JudgementResult,
    pub baseline: f64,
    pub observed: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub judged: Vec<Judge>,
    pub acks_refused: Vec<AckRefused>,
    pub steps: Vec<Planned>,
    pub would_step: Vec<Planned>,
    pub refused: Vec<Refused>,
    pub exhausted: Vec<String>,
    pub no_lever: Vec<NoLever>,
    pub terms: Vec<String>,
}

/// What one candidate was refused for. `code` is the metric label; the reason names what.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    StaleInput(String),
    NotCaughtUp,
    BackOff { until: u64 },
    InFlight(String),
    UpstreamAbsent(String),
    UpstreamStale(String),
    UpstreamGapped(String),
    NoAndon,
    AndonPulled(String),
    Stopping,
    KnobAbsent(String),
    KnobNotNumeric(String),
    MovesUnreadable(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::StaleInput(_) => "stale-input",
            Refusal::NotCaughtUp => "not-caught-up",
            Refusal::BackOff { .. } => "back-off",
            Refusal::InFlight(_) => "in-flight",
            Refusal::UpstreamAbsent(_) => "upstream-absent",
            Refusal::UpstreamStale(_) => "upstream-stale",
            Refusal::UpstreamGapped(_) => "upstream-gapped",
            Refusal::NoAndon => "no-andon",
            Refusal::AndonPulled(_) => "andon-pulled",
            Refusal::Stopping => "stopping",
            Refusal::KnobAbsent(_) => "knob-absent",
            Refusal::KnobNotNumeric(_) => "knob-not-numeric",
            Refusal::MovesUnreadable(_) => "moves-unreadable",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Refusal::StaleInput(s) => format!("stale input: {s}"),
            Refusal::NotCaughtUp => "the target section has not caught up with its spec".into(),
            Refusal::BackOff { until } => format!("in back-off until {}", format_rfc3339(*until)),
            Refusal::InFlight(k) => format!("{k} is in flight in this neighbourhood"),
            Refusal::UpstreamAbsent(r) => format!("upstream absent: no section produces {r}"),
            Refusal::UpstreamStale(s) => format!("upstream stale: {s}"),
            Refusal::UpstreamGapped(s) => format!("upstream gapped: {s}"),
            Refusal::NoAndon => "a destructive step needs an andon section, and none exists".into(),
            Refusal::AndonPulled(s) => format!("the andon cord is pulled on {s}"),
            Refusal::Stopping => "stops belong to the andon cord".into(),
            Refusal::KnobAbsent(k) => format!("{k} is absent, so no from can be read"),
            Refusal::KnobNotNumeric(k) => format!("{k} is not a number"),
            Refusal::MovesUnreadable(f) => format!("{f} has no value to take a baseline from"),
        }
    }
}

/// `horizon × 2^k` for `k` consecutive failures, capped at a day.
pub fn backoff_secs(horizon: u64, k: u32) -> u64 {
    2u64.checked_pow(k).and_then(|m| horizon.checked_mul(m)).map_or(BACKOFF_CAP_SECS, |b| b.min(BACKOFF_CAP_SECS))
}

/// The knob a section's action moves: under the section's own `knobs/`, which only its owner
/// applies to.
pub fn knob_key(section: &str, action: &str) -> String {
    format!("knobs/{section}/{action}")
}

fn section_of(key: &str) -> Option<String> {
    parse_key(key).map(|(k, n, _)| format!("{k}/{n}"))
}

fn kind_of(section: &str) -> &str {
    section.split_once('/').map_or(section, |(k, _)| k)
}

fn value_of(cur: &Current, field: &str) -> Option<f64> {
    cur.status.fields.get(field)?.get("value")?.as_f64()
}

fn fresh(cur: Option<&Current>) -> bool {
    cur.is_some_and(|c| c.freshness == Freshness::Fresh)
}

fn horizon_end(i: &Intent) -> Option<u64> {
    Some(parse_rfc3339(&i.taken_at)? + parse_duration(&i.horizon)?)
}

/// Whether moving from `baseline` to `observed` meets `expected` in its direction.
fn met(baseline: f64, observed: f64, expected: f64) -> bool {
    let moved = observed - baseline;
    if expected < 0.0 {
        moved <= expected
    } else {
        moved >= expected
    }
}

/// One intent read back: what the selector learns from it.
enum ReadBack {
    /// It holds its neighbourhood until it is judged.
    InFlight,
    /// Unacknowledged past its horizon: the assembler marked the owner; nothing is judged.
    Unacknowledged,
    Refused,
    Judged { result: JudgementResult, at: u64 },
}

struct History<'a> {
    a: &'a Assembled,
    now: u64,
}

impl History<'_> {
    fn read_back(&self, i: &Intent, section: &str, plan: &mut Plan, key: &str) -> ReadBack {
        if let Some(j) = &i.judgement {
            return ReadBack::Judged { result: j.result, at: parse_rfc3339(&j.at).unwrap_or(self.now) };
        }
        let end = horizon_end(i);
        let Some(ack) = &i.ack else {
            return if end.is_some_and(|e| self.now <= e) { ReadBack::InFlight } else { ReadBack::Unacknowledged };
        };
        if let AckResult::Refused { reason, .. } = &ack.result {
            let since_prev = self.a.prev.as_ref().and_then(|p| parse_rfc3339(&p.at));
            let at = parse_rfc3339(&ack.at);
            if since_prev.is_none_or(|p| at.is_none_or(|a| a > p)) {
                plan.acks_refused.push(AckRefused { intent: key.to_string(), reason: reason.clone() });
            }
            return ReadBack::Refused;
        }
        if end.is_some_and(|e| self.now <= e) {
            return ReadBack::InFlight;
        }
        let cur = self.a.current.sections.get(section);
        let observed = cur.filter(|c| c.freshness == Freshness::Fresh).and_then(|c| value_of(c, &i.moves));
        let Some(observed) = observed else {
            return ReadBack::InFlight;
        };
        let result = if met(i.baseline, observed, i.expected_effect) {
            JudgementResult::Fulfilled
        } else {
            JudgementResult::Failed
        };
        plan.judged.push(Judge { key: key.to_string(), section: section.to_string(), result, baseline: i.baseline, observed });
        ReadBack::Judged { result, at: self.now }
    }
}

/// Per (section, action, target): the attempts in the order they were taken.
#[derive(Default)]
struct Attempts {
    last_taken: u64,
    /// (taken_at, judged result, judged at) for each judged attempt.
    judged: Vec<(u64, JudgementResult, u64)>,
    horizon: u64,
}

impl Attempts {
    /// When back-off ends, if it has not.
    fn backoff_until(&self, now: u64) -> Option<u64> {
        let mut sorted = self.judged.clone();
        sorted.sort_by_key(|(taken, _, _)| *taken);
        let k = sorted.iter().rev().take_while(|(_, r, _)| *r == JudgementResult::Failed).count();
        let (_, _, at) = sorted.last().filter(|_| k > 0)?;
        let until = at + backoff_secs(self.horizon, u32::try_from(k).unwrap_or(u32::MAX));
        (now < until).then_some(until)
    }
}

/// A flow graph over the assembled sections: who produces each resource.
struct Flow<'a> {
    a: &'a Assembled,
}

impl Flow<'_> {
    fn producers(&self, resource: &str) -> Vec<&str> {
        self.a
            .current
            .sections
            .iter()
            .filter(|(_, c)| c.flow.produces.iter().any(|p| p == resource))
            .map(|(r, _)| r.as_str())
            .collect()
    }

    fn consumes(&self, section: &str) -> &[String] {
        self.a.current.sections.get(section).map_or(&[], |c| c.flow.consumes.as_slice())
    }

    /// A step's neighbourhood is its target and, for a capacity step, every resource its section
    /// consumes and every section that produces one.
    fn neighbourhood(&self, section: &str, class: ActionClass, target: &str) -> BTreeSet<String> {
        let mut n = BTreeSet::from([format!("target:{target}")]);
        if matches!(class, ActionClass::CapacityRaise | ActionClass::CapacityLower) {
            for r in self.consumes(section) {
                n.insert(format!("resource:{r}"));
                for p in self.producers(r) {
                    n.insert(format!("section:{p}"));
                }
            }
        }
        n
    }

    fn neighbour_sections(&self, section: &str, class: ActionClass) -> Vec<&str> {
        match class {
            ActionClass::CapacityRaise | ActionClass::CapacityLower => {
                self.consumes(section).iter().flat_map(|r| self.producers(r)).collect()
            }
            ActionClass::Local | ActionClass::Destructive => Vec::new(),
        }
    }
}

fn open_conditions(cur: &Current) -> Vec<String> {
    let mut open: Vec<String> = cur
        .status
        .conditions
        .iter()
        .filter(|c| c.status == ConditionStatus::True)
        .map(|c| c.type_.clone())
        .collect();
    for d in &cur.derived {
        match d {
            Derived::IntentsUnacknowledged { .. } => open.push(INTENTS_UNACKNOWLEDGED.into()),
        }
    }
    open
}

fn andon_sections(a: &Assembled) -> Vec<&Current> {
    a.current.sections.iter().filter(|(r, _)| kind_of(r) == ANDON).map(|(_, c)| c).collect()
}

/// A pulled cord names what it stops in its condition's reason, as a section ref or a knob.
fn andon_pulled_on(a: &Assembled, section: &str, target: &str) -> bool {
    andon_sections(a).iter().any(|c| {
        c.status
            .conditions
            .iter()
            .any(|cond| cond.status == ConditionStatus::True && (cond.reason == section || cond.reason == target))
    })
}

/// `to` for a closed-form step from `from`. Integers stay integers, so the owner's compare
/// of the knob against `from` sees the value it holds.
pub fn step_to(step: &str, from: &Value) -> Option<Value> {
    let f = from.as_f64()?;
    let integral = from.is_i64() || from.is_u64();
    let words: Vec<&str> = step.split_whitespace().collect();
    let num = |s: &str| s.parse::<f64>().ok();
    let to = match words.as_slice() {
        ["one", "fewer", ..] => f - 1.0,
        ["one", ..] => f + 1.0,
        [pct, dir] if pct.ends_with('%') && (*dir == "more" || *dir == "fewer") => {
            let n = num(&pct[..pct.len() - 1])? / 100.0;
            let raw = if *dir == "more" { f * (1.0 + n) } else { f * (1.0 - n) };
            if integral {
                let r = raw.round();
                // An asymptotic step still moves an integer knob by one.
                if r == f {
                    if *dir == "more" { f + 1.0 } else { f - 1.0 }
                } else {
                    r
                }
            } else {
                raw
            }
        }
        [n, ..] => {
            if let Some(d) = n.strip_prefix('+') {
                f + num(d)?
            } else {
                f - num(n.strip_prefix('-').or_else(|| n.strip_prefix('−'))?)?
            }
        }
        [] => return None,
    };
    let to = if integral { to.max(0.0) } else { to };
    if integral && to.fract() == 0.0 {
        return Some(Value::from(to as i64));
    }
    serde_json::Number::from_f64(to).map(Value::Number)
}

struct Selector<'a> {
    inp: &'a Inputs<'a>,
    flow: Flow<'a>,
    now: u64,
    attempts: BTreeMap<(String, String, String), Attempts>,
    /// Neighbourhood members held by an intent in flight, and the intent holding each.
    held: BTreeMap<String, String>,
    open: BTreeSet<&'a str>,
}

impl<'a> Selector<'a> {
    fn eligible(&self, section: &str, cur: &Current, act: &Action) -> Result<Planned, Refusal> {
        let a = self.inp.assembled;
        if cur.freshness != Freshness::Fresh {
            return Err(Refusal::StaleInput(section.into()));
        }
        if !a.ages.get(section).is_some_and(|g| g.caught_up) {
            return Err(Refusal::NotCaughtUp);
        }
        if act.class == ActionClass::CapacityRaise {
            for r in self.flow.consumes(section) {
                let producers = self.flow.producers(r);
                if producers.is_empty() {
                    return Err(Refusal::UpstreamAbsent(r.clone()));
                }
                for p in producers {
                    if !fresh(a.current.sections.get(p)) {
                        return Err(Refusal::UpstreamStale(p.into()));
                    }
                    if self.open.contains(p) {
                        return Err(Refusal::UpstreamGapped(p.into()));
                    }
                }
            }
        }
        for n in self.flow.neighbour_sections(section, act.class) {
            if !fresh(a.current.sections.get(n)) {
                return Err(Refusal::StaleInput(n.into()));
            }
        }
        let andons = andon_sections(a);
        if let Some(stale) = andons.iter().find(|c| c.freshness != Freshness::Fresh) {
            return Err(Refusal::StaleInput(format!("{ANDON}/{}", stale.metadata.name)));
        }
        if act.stopping == Some(true) {
            return Err(Refusal::Stopping);
        }
        let target = knob_key(section, &act.name);
        let at = self.attempts.get(&(section.to_string(), act.name.clone(), target.clone()));
        if let Some(until) = at.and_then(|h| h.backoff_until(self.now)) {
            return Err(Refusal::BackOff { until });
        }
        let hood = self.flow.neighbourhood(section, act.class, &target);
        if let Some(k) = hood.iter().find_map(|m| self.held.get(m)) {
            return Err(Refusal::InFlight(k.clone()));
        }
        if act.class == ActionClass::Destructive && andons.is_empty() {
            return Err(Refusal::NoAndon);
        }
        if andon_pulled_on(a, section, &target) {
            return Err(Refusal::AndonPulled(section.into()));
        }
        let from = a.knobs.get(&target).ok_or_else(|| Refusal::KnobAbsent(target.clone()))?;
        let to = step_to(&act.step, from).ok_or_else(|| Refusal::KnobNotNumeric(target.clone()))?;
        let baseline = value_of(cur, &act.moves).ok_or_else(|| Refusal::MovesUnreadable(act.moves.clone()))?;
        let id = format!("p{}-r{}-{}", a.n, a.list_rev, act.name);
        let (kind, name) = section.split_once('/').unwrap_or((section, ""));
        let intent = Intent {
            id: id.clone(),
            action: act.name.clone(),
            target: target.clone(),
            from: from.clone(),
            to,
            moves: act.moves.clone(),
            baseline,
            expected_effect: act.expected_effect,
            horizon: act.horizon.clone(),
            addresses: act.addresses.clone(),
            taken_at: format_rfc3339(self.now),
            by: By { pass: a.n.to_string(), lock_token: self.inp.token.to_string() },
            ack: None,
            judgement: None,
        };
        let held = if self.inp.halted {
            Some("world halted".to_string())
        } else if crate::effect::is_shadowed(self.inp.shadow, kind) {
            Some(format!("shadow: {kind}"))
        } else {
            None
        };
        Ok(Planned {
            section: section.into(),
            action: act.name.clone(),
            key: spira_desired_state::intent::intent_key(kind, name, &id),
            intent,
            held,
        })
    }

    fn claim(&mut self, section: &str, act: &Action, key: &str) {
        let target = knob_key(section, &act.name);
        for m in self.flow.neighbourhood(section, act.class, &target) {
            self.held.insert(m, key.to_string());
        }
    }

    fn last_attempt(&self, section: &str, act: &str) -> u64 {
        self.attempts
            .iter()
            .filter(|((s, a, _), _)| s == section && a == act)
            .map(|(_, h)| h.last_taken)
            .max()
            .unwrap_or(0)
    }

    fn gap(&mut self, section: &str, plan: &mut Plan) {
        let a = self.inp.assembled;
        let Some(cur) = a.current.sections.get(section) else { return };
        if cur.actions.is_empty() {
            plan.terms.push(section.into());
            return;
        }
        let open = open_conditions(cur);
        let addressed = |c: &String| cur.actions.iter().any(|act| act.addresses.contains(c));
        let unaddressed: Vec<String> = open.iter().filter(|c| !addressed(c)).cloned().collect();
        let mut candidates: Vec<&Action> = cur.actions.iter().filter(|act| act.addresses.iter().any(|c| open.contains(c))).collect();
        let owner_silent = unaddressed.iter().any(|c| c == INTENTS_UNACKNOWLEDGED);
        if !unaddressed.is_empty() || candidates.is_empty() {
            let since = a
                .prev
                .as_ref()
                .and_then(|p| p.no_lever.iter().find(|l| l.section == section && l.conditions == unaddressed))
                .map_or(a.n, |l| l.since);
            plan.no_lever.push(NoLever { section: section.into(), conditions: unaddressed, since });
        }
        // An owner that has not acknowledged its intents takes no more of them.
        if candidates.is_empty() || owner_silent {
            return;
        }
        candidates.sort_by(|x, y| (self.last_attempt(section, &x.name), &x.name).cmp(&(self.last_attempt(section, &y.name), &y.name)));
        let mut refusals = Vec::new();
        for act in candidates {
            match self.eligible(section, cur, act) {
                Ok(p) => {
                    self.claim(section, act, &p.key);
                    if p.held.is_some() {
                        plan.would_step.push(p);
                    } else {
                        plan.steps.push(p);
                    }
                    return;
                }
                Err(r) => refusals.push((act.name.clone(), r)),
            }
        }
        if refusals.iter().all(|(_, r)| matches!(r, Refusal::BackOff { .. })) {
            plan.exhausted.push(section.into());
            return;
        }
        for (action, r) in refusals {
            plan.refused.push(Refused { section: section.into(), action, code: r.code().into(), reason: r.reason() });
        }
    }
}

fn hold(held: &mut BTreeMap<String, String>, flow: &Flow, section: &str, class: ActionClass, key: &str, target: &str) {
    for m in flow.neighbourhood(section, class, target) {
        held.insert(m, key.to_string());
    }
}

pub fn select(inp: &Inputs) -> Plan {
    let a = inp.assembled;
    let now = inp.now_ms / 1000;
    let mut plan = Plan::default();
    let history = History { a, now };
    let flow = Flow { a };
    let mut attempts: BTreeMap<(String, String, String), Attempts> = BTreeMap::new();
    let mut held = BTreeMap::new();
    for li in &a.intents {
        let Some(section) = section_of(&li.key) else { continue };
        let i = &li.intent;
        let rb = history.read_back(i, &section, &mut plan, &li.key);
        let h = attempts.entry((section.clone(), i.action.clone(), i.target.clone())).or_default();
        let taken = parse_rfc3339(&i.taken_at).unwrap_or(0);
        if taken >= h.last_taken {
            h.horizon = parse_duration(&i.horizon).unwrap_or(BACKOFF_CAP_SECS);
        }
        h.last_taken = h.last_taken.max(taken);
        // An action no longer declared holds as widely as a capacity step could.
        let class = a
            .current
            .sections
            .get(&section)
            .and_then(|c| c.actions.iter().find(|act| act.name == i.action))
            .map_or(ActionClass::CapacityRaise, |act| act.class);
        let capacity = matches!(class, ActionClass::CapacityRaise | ActionClass::CapacityLower);
        match rb {
            ReadBack::InFlight => hold(&mut held, &flow, &section, class, &li.key, &i.target),
            // A late owner may still apply a capacity step, so it keeps the shared resource.
            ReadBack::Unacknowledged if capacity => hold(&mut held, &flow, &section, class, &li.key, &i.target),
            ReadBack::Judged { result, at } => h.judged.push((taken, result, at)),
            ReadBack::Unacknowledged | ReadBack::Refused => {}
        }
    }
    let open: BTreeSet<&str> = inp.gaps.iter().filter(|g| g.open).map(|g| g.section.as_str()).collect();
    let mut s = Selector { inp, flow, now, attempts, held, open };
    for g in inp.gaps.iter().filter(|g| g.open) {
        s.gap(&g.section, &mut plan);
    }
    plan
}

#[cfg(test)]
mod tests;
