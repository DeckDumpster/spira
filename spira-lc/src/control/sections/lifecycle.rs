//! `publish-section lifecycle`: spira-lc's own section. Goodput from typed loop events, the
//! CERTIFIED dwell, and `BelowFloor`, published through `section publish-status` when a status
//! field changed, and otherwise every `target_latency` as a keepalive.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use lifecycle::bead::BeadState;
use serde_json::{json, Value};
use spira_desired_state::section::{
    section_key, Condition, ConditionStatus, Distance, Flow, Numeric, Section, SectionMeta,
    Unobservable, WarmingUp, Windowed, API_VERSION,
};
use spira_desired_state::sections::lifecycle::{Evidence, Goodput, Harm, Signal, Status, BELOW_FLOOR, NAME, OWNER, SECTION};
use spira_otel::metrics::Point;
use state_store::Backend;

use super::{cannot, publish, read_section, record, render, rfc3339, Answer, Call};
use crate::control::store::Store;
use crate::goodput::{self, LoopEvent, Measure, Scope, StopKind, Window, DAY_SECS};

pub const VERB: &str = "publish-section";
const HOUR_SECS: u64 = 3600;
const TARGET_LATENCY: &str = "5m";
const TARGET_LATENCY_SECS: u64 = 300;
const MAX_AGE: &str = "15m";
/// Below a minute, so a 60 s timer's jitter never skips every other tick.
const MIN_GAP_SECS: u64 = 50;
const DISTANCE_OPEN: f64 = 0.05;
const DISTANCE_CLOSE: f64 = 0.02;
const ALL: &str = "all";

/// The loop events spira-lc does not hold itself.
pub trait Outside {
    /// Halts, releases and landing stops, whenever they happened.
    fn stops(&self) -> Result<Vec<LoopEvent>, String>;
    /// Attributed reds, ejects and over-cap passes stamped at or after `since`.
    fn rounds(&self, since: u64) -> Result<Vec<LoopEvent>, String>;
    /// Whether a release of the world is recorded. Without it a past halt is invisible, its
    /// time reads as running, and the trailing day drains to 0 after every release.
    fn records_releases(&self) -> bool;
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call, outside: &dyn Outside) -> (i32, String) {
    let answer = match args.get(1).map(String::as_str) {
        Some("lifecycle") => publish_lifecycle(store, call, outside),
        other => cannot(format!("{other:?} is not a section spira-lc publishes: publish-section lifecycle")),
    };
    if !matches!(&answer, Answer::Ok(v) if v["published"] == false) {
        record(call.metrics, VERB, NAME, &answer);
    }
    render(answer)
}

fn windowed(m: &Measure, window: &str) -> Windowed {
    match m {
        Measure::Value(v) => Windowed::Value(Numeric { value: *v, window: window.into() }),
        Measure::Unobservable(why) => Windowed::Unobservable(Unobservable { unobservable: (*why).into(), window: window.into() }),
        Measure::WarmingUp => Windowed::WarmingUp(WarmingUp::WarmingUp),
    }
}

fn fields(hour: &goodput::Goodput, day: &goodput::Goodput) -> Goodput {
    Goodput {
        goodput_ratio_1h: Some(windowed(&hour.ratio, "1h")),
        goodput_rate_1h: Some(windowed(&hour.rate, "1h")),
        goodput_ratio_24h: Some(windowed(&day.ratio, "24h")),
        goodput_rate_24h: Some(windowed(&day.rate, "24h")),
        landed_1h: Some(windowed(&hour.landed, "1h")),
        landed_24h: Some(windowed(&day.landed, "24h")),
    }
}

fn signal(s: goodput::Signal) -> Signal {
    match s {
        goodput::Signal::LandingStop => Signal::LandingStop,
        goodput::Signal::Revert => Signal::Revert,
        goodput::Signal::RedWithInnocents => Signal::RedWithInnocents,
        goodput::Signal::RepeatEject => Signal::RepeatEject,
        goodput::Signal::OverCap => Signal::OverCap,
    }
}

fn minutes(secs: u64) -> f64 {
    secs as f64 / 60.0
}

fn harm(h: &goodput::Harm, repo_of: &BTreeMap<String, String>) -> Harm {
    Harm {
        bead: h.bead.clone(),
        repo: repo_of.get(&h.bead).cloned(),
        cost_minutes: minutes(h.cost_secs),
        evidence: h
            .evidence
            .iter()
            .map(|e| Evidence {
                signal: signal(e.signal),
                at: rfc3339(e.at * 1000),
                minutes: minutes(e.secs),
                batch: e.batch.clone(),
            })
            .collect(),
    }
}

/// Each stay in CERTIFIED that ended, as `(left_at, secs)`, from one bead's applied history.
fn certified_stays(changes: &[crate::control::feed::Change]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut entered: Option<i64> = None;
    for a in changes.iter().map(|c| &c.audit).filter(|a| a.applied) {
        let certified = BeadState::Certified.as_str();
        if a.to_state == certified && a.from_state != certified {
            entered = Some(a.at);
        } else if a.from_state == certified && a.to_state != certified {
            if let Some(t) = entered.take() {
                out.push((a.at.max(0) as u64, (a.at - t).max(0) as u64));
            }
        }
    }
    out
}

/// The repos any windowed event names.
fn repos_in(events: &[LoopEvent], day: &Window, repo_of: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in events {
        let (repo, at) = match e {
            LoopEvent::Landed { repo, at, .. } | LoopEvent::OverCapPass { repo, at, .. } => (Some(repo.clone()), *at),
            LoopEvent::Stop { repo: Some(r), at, .. } => (Some(r.clone()), *at),
            LoopEvent::Stop { .. } => continue,
            LoopEvent::RedAttributed { bead, at, .. } | LoopEvent::Eject { bead, at, .. } => (repo_of.get(bead).cloned(), *at),
        };
        if let Some(r) = repo.filter(|r| !r.is_empty() && day.contains(at)) {
            out.insert(r);
        }
    }
    out
}

fn spec_value(s: Option<&Section>, name: &str) -> Option<f64> {
    s?.spec.get(name)?.value.as_f64()
}

fn below_floor(rate: &Measure, floor: Option<f64>, until: Option<u64>, releases: bool) -> (ConditionStatus, String) {
    match (rate, floor) {
        (_, None) => (ConditionStatus::Unknown, "no goodput_rate_floor in this section's spec".into()),
        _ if !releases => (
            ConditionStatus::Unknown,
            "no release of the world is recorded, so the trailing day cannot exclude a past halt".into(),
        ),
        (Measure::Value(v), Some(f)) if *v < f => (
            ConditionStatus::True,
            format!("goodput rate {v:.1}/day over 24h of unhalted time is below the floor {f}"),
        ),
        (Measure::Value(v), Some(f)) => (
            ConditionStatus::False,
            format!("goodput rate {v:.1}/day over 24h of unhalted time meets the floor {f}"),
        ),
        (_, Some(_)) => (
            ConditionStatus::False,
            format!("warming up after a release of the world, until {}", rfc3339(until.unwrap_or_default() * 1000)),
        ),
    }
}

fn distance(rate: &Measure, floor: Option<f64>, dwell: &Measure, dwell_target: Option<f64>) -> Option<Distance> {
    let mut terms = Vec::new();
    if let (Measure::Value(_), Some(f)) = (rate, floor.filter(|f| *f > 0.0)) {
        terms.push(format!("max(0, 1 - goodput_rate_24h / {f})"));
    }
    if let (Measure::Value(_), Some(t)) = (dwell, dwell_target.filter(|t| *t > 0.0)) {
        terms.push(format!("max(0, certified_dwell_p95_secs / {t} - 1)"));
    }
    (!terms.is_empty()).then(|| Distance {
        expr: terms.join(" + "),
        open: DISTANCE_OPEN,
        close: DISTANCE_CLOSE,
    })
}

fn push_metrics(call: &Call, repo: &str, hour: &goodput::Goodput, day: &goodput::Goodput) {
    for (window, g) in [("1h", hour), ("24h", day)] {
        let mut points = Vec::new();
        for (name, m) in [("spira_goodput_ratio", &g.ratio), ("spira_goodput_rate", &g.rate), ("spira_goodput_landed", &g.landed)] {
            if let Measure::Value(value) = m {
                points.push(Point::Gauge { name, value: *value });
            }
        }
        if !points.is_empty() {
            call.metrics.push(&points, &[("repo", repo), ("window", window)]);
        }
    }
}

fn publish_lifecycle<B: Backend>(store: &Store<B>, call: &Call, outside: &dyn Outside) -> Answer {
    let now = call.now_ms / 1000;
    let key = section_key(NAME, SECTION);
    let existing = match read_section(store, &key) {
        Ok(e) => e,
        Err(a) => return a,
    };
    let last = existing.as_ref().and_then(|(s, _)| s.metadata.observed_at.as_deref()).and_then(spira_desired_state::section::parse_rfc3339);
    if let Some(at) = last.filter(|at| now < at + MIN_GAP_SECS) {
        return Answer::Ok(json!({ "published": false, "reason": format!("published {}s ago", now.saturating_sub(at)) }));
    }

    let mut events = match outside.stops() {
        Ok(e) => e,
        Err(e) => return cannot(format!("the loop's stops: {e}")),
    };
    let day = Window::new(&events, now, DAY_SECS);
    let hour = Window::new(&events, now, HOUR_SECS);
    match outside.rounds(day.start.saturating_sub(DAY_SECS)) {
        Ok(r) => events.extend(r),
        Err(e) => return cannot(format!("the loop's rounds: {e}")),
    }
    let records = match store.records() {
        Ok((r, _)) => r,
        Err(e) => return cannot(e),
    };
    let mut repo_of = BTreeMap::new();
    let mut stays = Vec::new();
    for r in &records {
        let row = &r.value.row;
        if let Some(repo) = &r.value.repo {
            repo_of.insert(row.bead_id.clone(), repo.clone());
        }
        let since = row.since.map(|s| s.max(0) as u64);
        match (row.state, since) {
            (BeadState::Landed, Some(at)) => {
                events.push(LoopEvent::Landed {
                    bead: row.bead_id.clone(),
                    repo: r.value.repo.clone().unwrap_or_default(),
                    at,
                    reverts: None,
                    subject: String::new(),
                });
                if day.contains(at) {
                    match store.history(&row.bead_id, Some("bead")) {
                        Ok(h) => stays.extend(certified_stays(&h)),
                        Err(e) => return cannot(e),
                    }
                }
            }
            (BeadState::Certified, Some(at)) => stays.push((now, now.saturating_sub(at))),
            _ => {}
        }
    }

    let total = (goodput::goodput(&events, &hour, Scope::All, &repo_of), goodput::goodput(&events, &day, Scope::All, &repo_of));
    let dwell = goodput::dwell_p95(&stays, &day);
    let mut status = Status {
        certified_dwell_p95_secs: Some(windowed(&dwell, "24h")),
        harmful: total.1.harmful.iter().map(|h| harm(h, &repo_of)).collect(),
        warming_up_until: day.warming_until.max(hour.warming_until).map(|u| rfc3339(u * 1000)),
        ..Status::default()
    };
    status.set_total(fields(&total.0, &total.1));
    push_metrics(call, ALL, &total.0, &total.1);
    for repo in repos_in(&events, &day, &repo_of) {
        let scope = Scope::Repo(&repo);
        let g = (goodput::goodput(&events, &hour, scope, &repo_of), goodput::goodput(&events, &day, scope, &repo_of));
        push_metrics(call, &repo, &g.0, &g.1);
        status.repos.insert(repo, fields(&g.0, &g.1));
    }

    let prev = existing.as_ref().map(|(s, _)| s);
    let floor = spec_value(prev, "goodput_rate_floor");
    let releases = outside.records_releases();
    let (cond, reason) = below_floor(&total.1.rate, floor, day.warming_until, releases);
    let since = prev
        .and_then(|s| s.status.conditions.iter().find(|c| c.type_ == BELOW_FLOOR && c.status == cond))
        .map_or_else(|| rfc3339(call.now_ms), |c| c.last_transition_time.clone());
    let fields = serde_json::to_value(&status).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default().into_iter().collect();
    let mut section = Section {
        api_version: API_VERSION.into(),
        kind: NAME.into(),
        metadata: SectionMeta {
            name: SECTION.into(),
            owner: OWNER.into(),
            observed_at: Some(rfc3339(call.now_ms)),
            target_latency: Some(TARGET_LATENCY.into()),
            max_age: Some(MAX_AGE.into()),
            ..SectionMeta::default()
        },
        spec: Default::default(),
        status: Default::default(),
        flow: Flow {
            consumes: vec!["round-controller/landed".into()],
            produces: Vec::new(),
        },
        distance: distance(&total.1.rate, floor.filter(|_| releases), &dwell, spec_value(prev, "certified_dwell_p95_secs")),
        actions: Vec::new(),
    };
    section.status.observed_generation = prev.and_then(|s| s.metadata.generation).unwrap_or(0);
    section.status.fields = fields;
    section.status.conditions = vec![Condition {
        type_: BELOW_FLOOR.into(),
        status: cond,
        reason,
        last_transition_time: since,
    }];

    let unchanged = prev.is_some_and(|p| p.status == section.status && p.distance == section.distance);
    if let Some(at) = last.filter(|at| unchanged && now < at + TARGET_LATENCY_SECS) {
        return Answer::Ok(json!({ "published": false, "reason": format!("unchanged since {}s ago", now - at) }));
    }
    let rev = existing.as_ref().map_or(0, |(_, i)| i.mod_rev.main).to_string();
    let args: Vec<String> = ["--actor", OWNER, "--expect-rev", &rev].map(String::from).to_vec();
    publish(store, &args, call, section)
}

/// The production source: the run directory's own typed records. Until halts and landing stops
/// are recorded as a family, the world's halt is read from its stamp alone.
pub struct RunDir(pub PathBuf);

impl RunDir {
    fn rows(&self, family: &str) -> Result<Vec<Value>, String> {
        let src = tsd::query::Source { reader: None, root: &self.0 };
        match src.rows(family, 0) {
            Ok(rows) => Ok(rows),
            Err(f) if f.code == tsd::query::ERR_NO_FAMILY => Ok(Vec::new()),
            Err(f) => Err(f.message),
        }
    }
}

fn mtime_secs(m: &std::fs::Metadata) -> Result<u64, String> {
    m.modified()
        .map_err(|e| e.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}

impl Outside for RunDir {
    fn stops(&self) -> Result<Vec<LoopEvent>, String> {
        let stamp = self.0.join("world.halted");
        match std::fs::metadata(&stamp) {
            Ok(m) => Ok(vec![LoopEvent::Stop { kind: StopKind::Halt, repo: None, bead: None, at: mtime_secs(&m)?, reason: String::new() }]),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", stamp.display())),
        }
    }

    fn records_releases(&self) -> bool {
        false
    }

    fn rounds(&self, since: u64) -> Result<Vec<LoopEvent>, String> {
        let passes = self.rows(tsd::pass::FAMILY)?;
        let attributions = self.rows("round-attribution")?;
        Ok(round_events(&passes, &attributions, since))
    }
}

/// Over-cap passes from `cert-pass`; each bead a `round-attribution` row names as owner, once
/// per round, as an eject numbered across every round that named it and a red that cost the
/// round's other members and its wall time.
pub fn round_events(passes: &[Value], attributions: &[Value], since: u64) -> Vec<LoopEvent> {
    let mut out = Vec::new();
    let mut rounds: BTreeMap<String, (Vec<String>, u64)> = BTreeMap::new();
    for row in passes {
        let (Ok(p), Some(at)) = (tsd::pass::PassRecord::from_row(row), tsd::query::ts_of(row)) else { continue };
        let wall = p.capacity_wait_s + p.boot_s + p.build_s + p.suites_s;
        if p.cause == tsd::pass::Cause::OverCap && at >= since {
            out.push(LoopEvent::OverCapPass { repo: p.repo.clone(), at, wall_secs: wall });
        }
        rounds.insert(p.batch, (p.members, wall));
    }
    let mut named: Vec<(u64, String, Vec<String>)> = attributions
        .iter()
        .filter_map(|r| {
            let owners: Vec<String> = tsd::query::text(r, "owner")?.split(',').filter(|o| !o.is_empty()).map(str::to_string).collect();
            Some((tsd::query::ts_of(r)?, tsd::query::text(r, "round")?.to_string(), owners))
        })
        .filter(|(_, _, owners)| !owners.is_empty())
        .collect();
    named.sort();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut ejects: BTreeMap<String, u32> = BTreeMap::new();
    for (at, round, owners) in named {
        for bead in &owners {
            if !seen.insert((bead.clone(), round.clone())) {
                continue;
            }
            let n = ejects.entry(bead.clone()).or_default();
            *n += 1;
            if at < since {
                continue;
            }
            let (members, wall) = rounds.get(&round).cloned().unwrap_or_default();
            out.push(LoopEvent::Eject { bead: bead.clone(), batch: round.clone(), at, repeat_n: *n });
            out.push(LoopEvent::RedAttributed {
                bead: bead.clone(),
                batch: round.clone(),
                innocent: members.into_iter().filter(|m| !owners.contains(m)).collect(),
                at,
                wall_secs: wall,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
